// Copyright 2019-2022 Clemens Lutz
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use data_store::join_data::{JoinDataBuilder, JoinDataGenFn};
use datagen::relation::KeyAttribute;
use num_rational::Ratio;
use numa_gpu::runtime::allocator::MemType;
use numa_gpu::runtime::cpu_affinity::CpuAffinity;
use numa_gpu::runtime::hw_info::{cpu_codename, NvidiaDriverInfo};
use numa_gpu::runtime::linux_wrapper;
use numa_gpu::runtime::numa::NodeRatio;
use numa_gpu::utils::DeviceType;
use radix_join::error::{ErrorKind, Result};
use radix_join::execution_methods::{
    cpu_partitioned_radix_join::cpu_partitioned_radix_join, gpu_radix_join::gpu_radix_join,
    gpu_triton_join::gpu_triton_join,
};
use radix_join::measurement::data_point::DataPoint;
use radix_join::measurement::harness::{self, RadixJoinPoint};
use radix_join::types::*;
use rustacuda::context::CurrentContext;
use rustacuda::device::Device;
use rustacuda::device::DeviceAttribute;
use rustacuda::function::{BlockSize, GridSize};
use rustacuda::memory::DeviceCopy;
use rustacuda::prelude::*;
use serde::de::DeserializeOwned;
use sql_ops::join::{cuda_radix_join, no_partitioning_join, HashingScheme};
use sql_ops::partition::cpu_radix_partition::{CpuHistogramAlgorithm, CpuRadixPartitionable};
use sql_ops::partition::gpu_radix_partition::{GpuHistogramAlgorithm, GpuRadixPartitionable};
use sql_ops::partition::{RadixBits, RadixPass};
use std::convert::TryInto;
use std::mem::size_of;
use std::path::PathBuf;
use structopt::StructOpt;

fn main() -> Result<()> {
    // Parse commandline arguments
    let mut cmd = CmdOpt::from_args();

    // Initialize CUDA
    rustacuda::init(CudaFlags::empty())?;
    let device = Device::get_device(cmd.device_id.into())?;
    let _context =
        Context::create_and_push(ContextFlags::MAP_HOST | ContextFlags::SCHED_AUTO, device)?;

    let cache_node = device.numa_node().ok();
    // println!("cache_node is {:?}", cache_node);
    let overflow_node = match device.numa_memory_affinity() {
        Ok(numa_node) => {
            // println!("@@@ NUMA OK");
            numa_node
        },
        Err(e) => {
            eprintln!("Warning: {}; Falling back to node = 0", e);
            0
        }
    };
    // println!("overflow_node is {:?}", overflow_node);


    cmd.set_state_mem(cache_node);
    cmd.set_partitions_mem(cache_node, overflow_node)?;

    match cmd.tuple_bytes {
        ArgTupleBytes::Bytes8 => {
            let (hjc, dp) = args_to_bench::<i32>(&cmd, device)?;
            harness::measure("radix_join", cmd.repeat, cmd.csv, dp, hjc)?;
        }
        ArgTupleBytes::Bytes16 => {
            let (hjc, dp) = args_to_bench::<i64>(&cmd, device)?;
            harness::measure("radix_join", cmd.repeat, cmd.csv, dp, hjc)?;
        }
    };

    Ok(())
}

#[derive(StructOpt)]
#[structopt(
    name = "radix-join",
    about = "A partitioned hash join optimized for large-to-large joins on GPUs with fast interconnects"
)]
struct CmdOpt {
    /// Number of times to repeat benchmark
    #[structopt(short = "r", long = "repeat", default_value = "30")]
    repeat: u32,

    /// Output filename for measurement CSV file
    #[structopt(long = "csv", parse(from_os_str))]
    csv: Option<PathBuf>,

    /// Memory type with which to allocate data.
    //   unified: CUDA Unified memory (default)
    //   numa: NUMA-local memory on node specified with [inner,outer]-rel-location
    #[structopt(
        long = "rel-mem-type",
        default_value = "Unified",
        possible_values = &ArgMemType::variants(),
        case_insensitive = true
    )]
    mem_type: ArgMemType,

    /// Hashing scheme to use in hash table
    #[structopt(
        long = "hashing-scheme",
        default_value = "LinearProbing",
        possible_values = &ArgHashingScheme::variants(),
        case_insensitive = true
    )]
    hashing_scheme: ArgHashingScheme,

    /// Memory type with which to allocate the partitioned data
    ///
    /// If the `GpuTritonJoinTwoPass` execution method is specified, the default
    /// value is changed to `DistributedNuma` with the GPU node and closest CPU
    /// node specified as partitions location. The NUMA nodes can be specified
    /// explictly with `--partitions-mem-type DistributedNuma` and
    /// `--partitions-location 255,0`. Here, the first location specifies the
    /// cache node, and the second location specifies the overflow node.
    #[structopt(
        long = "partitions-mem-type",
        default_value = "Unified",
        possible_values = &ArgMemType::variants(),
        case_insensitive = true
    )]
    partitions_mem_type: ArgMemType,

    /// NUMA nodes on which the partitioned data is allocated
    ///
    /// NUMA nodes are specified as a list (e.g.: 0,1,2). See numactl -H for
    /// the available NUMA nodes.
    ///
    /// The NUMA node list is only used for the `Numa` and `DistributedNuma`
    /// memory types. Multiple nodes are only valid for the `DistributedNuma`
    /// memory type.
    #[structopt(
        long = "partitions-location",
        default_value = "0",
        require_delimiter = true
    )]
    partitions_location: Vec<u16>,

    /// Proportions with which the partitioned data are allocated on multiple nodes
    ///
    /// Given as a list of percentages (e.g.: 20,60,20), that should add up to
    /// 100%.
    ///
    /// The proportions are used only for the `DistributedNuma` memory type and
    /// have no effect on other memory types.
    #[structopt(
        long = "partitions-proportions",
        default_value = "100",
        require_delimiter = true
    )]
    partitions_proportions: Vec<usize>,

    /// Use NUMA memory to allocate the on-GPU state
    ///
    /// Execution methods require state to operate. This includes, for example,
    /// the partitioned inner and outer relations resulting from the second
    /// partitioning pass.
    ///
    /// By default, state is allocated as CUDA device memory. If this flag is
    /// enabled, state is instead allocated as NUMA memory in GPU memory using
    /// `mmap`.
    ///
    /// Warning: This option requires cache-coherence, and does not work on
    /// PCI-e devices.
    #[structopt(long = "use-numa-mem-state")]
    use_numa_mem_state: Option<bool>,

    #[structopt(skip = ArgMemType::Device)]
    state_mem_type: ArgMemType,

    #[structopt(skip)]
    state_location: u16,

    /// Allocate memory for inner relation on CPU or GPU (See numactl -H and CUDA device list)
    #[structopt(long = "inner-rel-location", default_value = "0")]
    inner_rel_location: u16,

    /// Allocate memory for outer relation on CPU or GPU (See numactl -H and CUDA device list)
    #[structopt(long = "outer-rel-location", default_value = "0")]
    outer_rel_location: u16,

    /// Page type with with to allocate memory
    #[structopt(
        long = "page-type",
        default_value = "Default",
        possible_values = &ArgPageType::variants(),
        case_insensitive = true
    )]
    page_type: ArgPageType,

    /// Use a pre-defined or custom data set.
    //   blanas: Blanas et al. "Main memory hash join algorithms for multi-core CPUs"
    //   blanas4mb: Blanas, but with a 4 MiB inner relation
    //   kim: Kim et al. "Sort vs. hash revisited"
    //   test: A small data set for testing on the laptop
    #[structopt(
        short = "s",
        long = "data-set",
        default_value = "Test",
        possible_values = &ArgDataSet::variants(),
        case_insensitive = true
    )]
    data_set: ArgDataSet,

    /// Outer relation's data distribution
    #[structopt(
        long = "data-distribution",
        default_value = "Uniform",
        possible_values = &ArgDataDistribution::variants(),
        case_insensitive = true
    )]
    data_distribution: ArgDataDistribution,

    /// Zipf exponent for Zipf-sampled outer relations
    #[structopt(long = "zipf-exponent", required_if("data-distribution", "Zipf"))]
    zipf_exponent: Option<f64>,

    /// Selectivity of the join, in percent
    #[structopt(
        long = "selectivity",
        default_value = "100",
        validator = is_percent
    )]
    selectivity: u32,

    /// Load data set from a TSV file with "key value" pairs and automatic gzip decompression
    #[structopt(
        long = "inner-rel-file",
        parse(from_os_str),
        conflicts_with = "data_set",
        requires = "outer_rel_file"
    )]
    inner_rel_file: Option<PathBuf>,

    /// Load data set from a TSV file with "key value" pairs and automatic gzip decompression
    #[structopt(
        long = "outer-rel-file",
        parse(from_os_str),
        conflicts_with = "data_set",
        requires = "inner_rel_file"
    )]
    outer_rel_file: Option<PathBuf>,

    /// Set the tuple size (bytes)
    #[structopt(
        long = "tuple-bytes",
        default_value = "Bytes8",
        possible_values = &ArgTupleBytes::variants(),
        case_insensitive = true
    )]
    tuple_bytes: ArgTupleBytes,

    /// Set the inner relation size (tuples); required for `-data-set Custom`
    #[structopt(long = "inner-rel-tuples", required_if("data_set", "Custom"))]
    inner_rel_tuples: Option<usize>,

    /// Set the outer relation size (tuples); required for `--data-set Custom`
    #[structopt(long = "outer-rel-tuples", required_if("data_set", "Custom"))]
    outer_rel_tuples: Option<usize>,

    /// Select the histogram algorithm for 1st pass
    #[structopt(
        long,
        default_value = "GpuChunked",
        possible_values = &ArgHistogramAlgorithm::variants(),
        case_insensitive = true
    )]
    histogram_algorithm: ArgHistogramAlgorithm,

    /// Select the histogram algorithm for 2nd pass
    #[structopt(
        long,
        default_value = "GpuContiguous",
        possible_values = &ArgHistogramAlgorithm::variants(),
        case_insensitive = true
    )]
    histogram_algorithm_2nd: ArgHistogramAlgorithm,

    /// Select the radix partition algorithm for 1st pass
    #[structopt(
        long,
        default_value = "GpuNC",
        possible_values = &ArgRadixPartitionAlgorithm::variants(),
        case_insensitive = true
    )]
    partition_algorithm: ArgRadixPartitionAlgorithm,

    /// Select the radix partition algorithm for 2nd pass
    #[structopt(
        long,
        default_value = "GpuNC",
        possible_values = &ArgRadixPartitionAlgorithm::variants(),
        case_insensitive = true
    )]
    partition_algorithm_2nd: ArgRadixPartitionAlgorithm,

    /// Join execution strategy.
    #[structopt(
        long = "execution-strategy",
        default_value = "GpuRadixJoinTwoPass",
        possible_values = &ArgExecutionMethod::variants(),
        case_insensitive = true
    )]
    execution_method: ArgExecutionMethod,

    #[structopt(
        long = "radix-bits",
        default_value = "8,8",
        parse(try_from_str = parse_radix_bits)
    )]
    /// Radix bits with which to partition
    radix_bits: RadixBits,

    #[structopt(short = "i", long = "device-id", default_value = "0")]
    /// Execute on GPU (See CUDA device list)
    device_id: u16,

    /// Device memory buffer sizes per partition per thread block for HSSWWC variants (in KiB)
    #[structopt(long, default_value = "8", require_delimiter = true)]
    dmem_buffer_size: usize,

    /// Device memory used to cache partitions in Triton join (upper limit, in MiB) [Default: All device memory]
    #[structopt(long)]
    max_partitions_cache_size: Option<usize>,

    #[structopt(short = "t", long = "threads", default_value = "1")]
    threads: usize,

    /// Path to CPU affinity map file for CPU workers
    #[structopt(long = "cpu-affinity", parse(from_os_str))]
    cpu_affinity: Option<PathBuf>,

    #[structopt(long = "grid-size", require_delimiter = true)]
    /// The CUDA grid size [Default: all SMs]
    grid_size: Option<u32>,
}

impl CmdOpt {
    fn set_state_mem(&mut self, state_location: Option<u16>) {
        self.state_mem_type = if let Some(true) = self.use_numa_mem_state {
            ArgMemType::Numa
        } else {
            ArgMemType::Device
        };
        self.state_location = state_location.unwrap_or_else(|| 0);
    }

    fn set_partitions_mem(
        &mut self,
        cache_location: Option<u16>,
        overflow_location: u16,
    ) -> Result<()> {
        if self.execution_method == ArgExecutionMethod::GpuTritonJoinTwoPass {
            if cache_location != None {
                let cache_location = cache_location.ok_or_else(|| {
                    ErrorKind::RuntimeError(
                        "Failed to set the cache NUMA location. Are you using PCI-e?".to_string(),
                    )
                })?;
    
                if ArgMemType::DistributedNuma != self.partitions_mem_type {
                    self.partitions_mem_type = ArgMemType::DistributedNuma;
                    self.partitions_location = vec![cache_location, overflow_location];
                    self.partitions_proportions = vec![0, 0];
                } else if self.partitions_location.len() != 2 {
                    let e = format!(
                        "Invalid argument: --partitions-location must specify \
                        exactly two locations when combined with --execution-method \
                        GpuTritonJoin\n\
                        The default locations are: --partitions-location {},{}",
                        cache_location, overflow_location
                    );
                    Err(ErrorKind::InvalidArgument(e))?;
                }
            }
            else {
                self.partitions_mem_type = ArgMemType::NumaPinned;
                self.partitions_location = vec![0, 0];
                self.partitions_proportions = vec![0, 0];
            }
        }

        Ok(())
    }
}

fn is_percent(x: String) -> std::result::Result<(), String> {
    x.parse::<i32>()
        .map_err(|_| {
            String::from(
                "Failed to parse integer. The value must be a percentage between [0, 100].",
            )
        })
        .and_then(|x| {
            if 0 <= x && x <= 100 {
                Ok(())
            } else {
                Err(String::from(
                    "The value must be a percentage between [0, 100].",
                ))
            }
        })
}

fn parse_radix_bits(input: &str) -> std::result::Result<RadixBits, String> {
    let radix_bits_ints: Vec<u32> = input
        .split(',')
        .map(str::trim)
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| format!("{}", e))?;

    let radix_bits = radix_bits_ints
        .as_slice()
        .try_into()
        .map_err(|e| format!("{}", e))?;

    Ok(radix_bits)
}

fn args_to_bench<T>(
    cmd: &CmdOpt,
    device: Device,
) -> Result<(Box<dyn FnMut() -> Result<RadixJoinPoint>>, DataPoint)>
where
    T: Default
        + Clone
        + Copy
        + DeviceCopy
        + Sync
        + Send
        + CpuRadixPartitionable
        + GpuRadixPartitionable
        + no_partitioning_join::CudaHashJoinable
        + no_partitioning_join::CpuHashJoinable
        + cuda_radix_join::CudaRadixJoinable
        + KeyAttribute
        + num_traits::FromPrimitive
        + DeserializeOwned,
{
    // Bind main thread to the CPU node closest to the GPU. This improves NVLink latency.
    match cmd.execution_method {
        ArgExecutionMethod::CpuPartitionedRadixJoinTwoPass
        | ArgExecutionMethod::GpuRadixJoinTwoPass
        | ArgExecutionMethod::GpuTritonJoinTwoPass => {
            let device = CurrentContext::get_device()?;
            if let Ok(local_cpu_node) = device.numa_memory_affinity() {
                linux_wrapper::numa_run_on_node(local_cpu_node).expect(&format!(
                    "Failed to bind main thread to CPU node {}",
                    local_cpu_node
                ));
                linux_wrapper::numa_set_preferred(local_cpu_node);
            } else {
                eprintln!(
                    "Warning: Couldn't bind main thread to the CPU closest to GPU {}. This may
                        cause additional latency in measurements.",
                    cmd.device_id
                );
            }
        }
    };

    // Device tuning
    let multiprocessors = device.get_attribute(DeviceAttribute::MultiprocessorCount)? as u32;
    let warp_size = device.get_attribute(DeviceAttribute::WarpSize)? as u32;
    let warp_overcommit_factor = 32;

    let block_size = BlockSize::x(warp_size * warp_overcommit_factor);
    let grid_size = GridSize::x(cmd.grid_size.unwrap_or(multiprocessors));
    let stream_grid_size = GridSize::x(grid_size.x / 2);

    let page_type = cmd.page_type;

    let mut data_builder = JoinDataBuilder::default();
    data_builder
        .mlock(true)
        .inner_mem_type(
            ArgMemTypeHelper {
                mem_type: cmd.mem_type,
                node_ratios: Box::new([NodeRatio {
                    node: cmd.inner_rel_location,
                    ratio: Ratio::from_integer(1),
                }]),
                page_type,
            }
            .into(),
        )
        .outer_mem_type(
            ArgMemTypeHelper {
                mem_type: cmd.mem_type,
                node_ratios: Box::new([NodeRatio {
                    node: cmd.outer_rel_location,
                    ratio: Ratio::from_integer(1),
                }]),
                page_type,
            }
            .into(),
        );

    let exec_method = cmd.execution_method;
    let histogram_algorithms: [DeviceType<CpuHistogramAlgorithm, GpuHistogramAlgorithm>; 2] = [
        cmd.histogram_algorithm.into(),
        cmd.histogram_algorithm_2nd.into(),
    ];
    let partition_algorithm: DeviceType<_, _> = cmd.partition_algorithm.into();
    let partition_algorithm_2nd: DeviceType<_, _> = cmd.partition_algorithm_2nd.into();
    let radix_bits = cmd.radix_bits;
    let dmem_buffer_bytes = cmd.dmem_buffer_size * 1024; // convert KiB to bytes
    let max_partitions_cache_bytes = cmd.max_partitions_cache_size.map(|s| s * 1024 * 1024); // convert MiB to bytes
    let mem_type = cmd.partitions_mem_type;
    let threads = cmd.threads;

    let state_mem_type = match cmd.state_mem_type {
        ArgMemType::Numa => MemType::NumaMem {
            node: cmd.state_location,
            page_type: page_type.into(),
        },
        ArgMemType::Device => MemType::CudaDevMem,
        _ => unreachable!(),
    };

    // Convert ArgHashingScheme to HashingScheme
    let hashing_scheme = HashingScheme::from(cmd.hashing_scheme);

    let node_ratios: Box<[NodeRatio]> = cmd
        .partitions_location
        .iter()
        .zip(cmd.partitions_proportions.iter())
        .map(|(node, pct)| NodeRatio {
            node: *node,
            ratio: Ratio::new(*pct, 100),
        })
        .collect();

    let partitions_mem_type: MemType = ArgMemTypeHelper {
        mem_type,
        node_ratios: node_ratios.clone(),
        page_type,
    }
    .into();

    // Load file or generate data set
    let (mut join_data, malloc_time, data_gen_time) =
        if let (Some(inner_rel_path), Some(outer_rel_path)) = (
            cmd.inner_rel_file.as_ref().and_then(|p| p.to_str()),
            cmd.outer_rel_file.as_ref().and_then(|p| p.to_str()),
        ) {
            data_builder.build_with_files::<T>(inner_rel_path, outer_rel_path)?
        } else {
            let data_distribution = match cmd.data_distribution {
                ArgDataDistribution::Uniform => DataDistribution::Uniform,
                ArgDataDistribution::Zipf => DataDistribution::Zipf(cmd.zipf_exponent.unwrap()),
            };

            let (inner_relation_len, outer_relation_len, data_gen) = data_gen_fn::<_>(
                cmd.data_set,
                cmd.inner_rel_tuples,
                cmd.outer_rel_tuples,
                data_distribution,
                Some(cmd.selectivity),
            );
            data_builder
                .inner_len(inner_relation_len)
                .outer_len(outer_relation_len)
                .build_with_data_gen(data_gen)?
        };

    // Construct data point template for CSV
    let dp = DataPoint::new()?
        .fill_from_cmd_options(cmd)?
        .fill_from_join_data(&join_data)
        .set_init_time(malloc_time, data_gen_time)
        .set_gpu_threads(&grid_size, &block_size);

    let cpu_affinity = if let Some(ref cpu_affinity_file) = cmd.cpu_affinity {
        CpuAffinity::from_file(cpu_affinity_file.as_path())?
    } else {
        CpuAffinity::default()
    };

    // Create closure that wraps a hash join benchmark function
    let hjc: Box<dyn FnMut() -> Result<RadixJoinPoint>> = match exec_method {
        ArgExecutionMethod::CpuPartitionedRadixJoinTwoPass => Box::new(move || {
            let (_result, data_point) = cpu_partitioned_radix_join(
                &mut join_data,
                hashing_scheme,
                histogram_algorithms[0],
                histogram_algorithms[1],
                partition_algorithm,
                partition_algorithm_2nd,
                &radix_bits,
                dmem_buffer_bytes,
                max_partitions_cache_bytes,
                threads,
                cpu_affinity.clone(),
                partitions_mem_type.clone(),
                state_mem_type.clone(),
                page_type.into(),
                (&grid_size, &block_size),
                (&stream_grid_size, &block_size),
            )?;

            Ok(data_point)
        }),
        ArgExecutionMethod::GpuRadixJoinTwoPass => Box::new(move || {
            let (_result, data_point) = gpu_radix_join(
                &mut join_data,
                hashing_scheme,
                histogram_algorithms[0],
                histogram_algorithms[1],
                partition_algorithm,
                partition_algorithm_2nd,
                &radix_bits,
                dmem_buffer_bytes,
                None,
                threads,
                cpu_affinity.clone(),
                partitions_mem_type.clone(),
                state_mem_type.clone(),
                page_type.into(),
                (&grid_size, &block_size),
                (&stream_grid_size, &block_size),
            )?;

            Ok(data_point)
        }),
        ArgExecutionMethod::GpuTritonJoinTwoPass => Box::new(move || {
            let (_result, data_point) = gpu_triton_join(
                &mut join_data,
                hashing_scheme,
                histogram_algorithms[0],
                histogram_algorithms[1],
                partition_algorithm,
                partition_algorithm_2nd,
                &radix_bits,
                dmem_buffer_bytes,
                max_partitions_cache_bytes,
                threads,
                cpu_affinity.clone(),
                partitions_mem_type.clone(),
                state_mem_type.clone(),
                page_type.into(),
                (&grid_size, &block_size),
                (&stream_grid_size, &block_size),
            )?;

            Ok(data_point)
        }),
    };

    Ok((hjc, dp))
}

fn data_gen_fn<T>(
    description: ArgDataSet,
    inner_rel_tuples: Option<usize>,
    outer_rel_tuples: Option<usize>,
    data_distribution: DataDistribution,
    selectivity: Option<u32>,
) -> (usize, usize, JoinDataGenFn<T>)
where
    T: Copy + Send + KeyAttribute + num_traits::FromPrimitive,
{
    match description {
        ArgDataSet::Blanas => (
            datagen::popular::Blanas::primary_key_len(),
            datagen::popular::Blanas::foreign_key_len(),
            Box::new(move |pk_rel, _, fk_rel, _| {
                datagen::popular::Blanas::gen(pk_rel, fk_rel, selectivity).map_err(|e| e.into())
            }),
        ),
        ArgDataSet::Kim => (
            datagen::popular::Kim::primary_key_len(),
            datagen::popular::Kim::foreign_key_len(),
            Box::new(move |pk_rel, _, fk_rel, _| {
                datagen::popular::Kim::gen(pk_rel, fk_rel, selectivity).map_err(|e| e.into())
            }),
        ),
        ArgDataSet::Blanas4MB => {
            let gen = move |pk_rel: &mut [_], _: &mut [_], fk_rel: &mut [_], _: &mut [_]| {
                datagen::relation::UniformRelation::gen_primary_key_par(pk_rel, selectivity)?;
                datagen::relation::UniformRelation::gen_attr_par(fk_rel, 0..pk_rel.len())?;
                Ok(())
            };

            (512 * 2_usize.pow(10), 256 * 2_usize.pow(20), Box::new(gen))
        }
        ArgDataSet::Test => {
            let gen = move |pk_rel: &mut [_], _: &mut [_], fk_rel: &mut [_], _: &mut [_]| {
                datagen::relation::UniformRelation::gen_primary_key(pk_rel, selectivity)?;
                datagen::relation::UniformRelation::gen_foreign_key_from_primary_key(
                    fk_rel, pk_rel,
                );
                Ok(())
            };

            (1000, 1000, Box::new(gen))
        }
        ArgDataSet::Lutz2Gv32G => {
            let gen = move |pk_rel: &mut [_], _: &mut [_], fk_rel: &mut [_], _: &mut [_]| {
                datagen::relation::UniformRelation::gen_primary_key_par(pk_rel, selectivity)?;
                datagen::relation::UniformRelation::gen_attr_par(fk_rel, 0..pk_rel.len())?;
                Ok(())
            };

            (
                2 * 2_usize.pow(30) / (2 * size_of::<T>()),
                32 * 2_usize.pow(30) / (2 * size_of::<T>()),
                Box::new(gen),
            )
        }
        ArgDataSet::Lutz32Gv32G => {
            let gen = move |pk_rel: &mut [_], _: &mut [_], fk_rel: &mut [_], _: &mut [_]| {
                datagen::relation::UniformRelation::gen_primary_key_par(pk_rel, selectivity)?;
                datagen::relation::UniformRelation::gen_attr_par(fk_rel, 0..pk_rel.len())?;
                Ok(())
            };

            (
                32 * 2_usize.pow(30) / (2 * size_of::<T>()),
                32 * 2_usize.pow(30) / (2 * size_of::<T>()),
                Box::new(gen),
            )
        }
        ArgDataSet::Custom => {
            let uniform_gen = Box::new(
                move |pk_rel: &mut [_], _: &mut [_], fk_rel: &mut [_], _: &mut [_]| {
                    datagen::relation::UniformRelation::gen_primary_key_par(pk_rel, selectivity)?;
                    datagen::relation::UniformRelation::gen_attr_par(fk_rel, 0..pk_rel.len())?;
                    Ok(())
                },
            );

            let gen: JoinDataGenFn<T> = match data_distribution {
                DataDistribution::Uniform => uniform_gen,
                DataDistribution::Zipf(exp) if !(exp > 0.0) => uniform_gen,
                DataDistribution::Zipf(exp) => Box::new(
                    move |pk_rel: &mut [_], _: &mut [_], fk_rel: &mut [_], _: &mut [_]| {
                        datagen::relation::UniformRelation::gen_primary_key_par(
                            pk_rel,
                            selectivity,
                        )?;
                        datagen::relation::ZipfRelation::gen_attr_par(fk_rel, pk_rel.len(), exp)?;
                        Ok(())
                    },
                ),
            };

            (
                inner_rel_tuples.expect(
                    "Couldn't find inner relation size. Did you specify --inner-rel-tuples?",
                ),
                outer_rel_tuples.expect(
                    "Couldn't find outer relation size. Did you specify --outer-rel-tuples?",
                ),
                gen,
            )
        }
    }
}

trait CmdOptToDataPoint {
    fn fill_from_cmd_options(&self, cmd: &CmdOpt) -> Result<DataPoint>;
}

impl CmdOptToDataPoint for DataPoint {
    fn fill_from_cmd_options(&self, cmd: &CmdOpt) -> Result<DataPoint> {
        // Get device information
        let dev_codename_str = match cmd.execution_method {
            ArgExecutionMethod::GpuRadixJoinTwoPass | ArgExecutionMethod::GpuTritonJoinTwoPass => {
                let device = Device::get_device(cmd.device_id.into())?;
                vec![device.name()?]
            } // CPU execution methods should use: vec![numa_gpu::runtime::hw_info::cpu_codename()?]
            ArgExecutionMethod::CpuPartitionedRadixJoinTwoPass => {
                let device = Device::get_device(cmd.device_id.into())?;
                vec![cpu_codename()?, device.name()?]
            }
        };
        println!("@@@@@@@@ Some(cmd.partitions_mem_type) is {:?}",Some(cmd.partitions_mem_type));
        let dp = DataPoint {
            data_set: Some(cmd.data_set.to_string()),
            histogram_algorithm: Some(cmd.histogram_algorithm),
            partition_algorithm: Some(cmd.partition_algorithm),
            partition_algorithm_2nd: Some(cmd.partition_algorithm_2nd),
            execution_method: Some(cmd.execution_method),
            device_codename: Some(dev_codename_str),
            dmem_buffer_size: Some(cmd.dmem_buffer_size),
            threads: Some(cmd.threads),
            radix_bits_fst: cmd.radix_bits.pass_radix_bits(RadixPass::First),
            radix_bits_snd: cmd.radix_bits.pass_radix_bits(RadixPass::Second),
            radix_bits_trd: cmd.radix_bits.pass_radix_bits(RadixPass::Third),
            hashing_scheme: Some(cmd.hashing_scheme),
            partitions_memory_type: Some(cmd.partitions_mem_type),
            partitions_memory_location: Some(cmd.partitions_location.clone()),
            partitions_proportions: Some(cmd.partitions_proportions.clone()),
            state_memory_type: Some(cmd.state_mem_type),
            state_memory_location: match cmd.state_mem_type {
                ArgMemType::Numa | ArgMemType::NumaPinned => Some(cmd.state_location),
                _ => None,
            },
            tuple_bytes: Some(cmd.tuple_bytes),
            relation_memory_type: Some(cmd.mem_type),
            page_type: Some(cmd.page_type),
            inner_relation_memory_location: Some(cmd.inner_rel_location),
            outer_relation_memory_location: Some(cmd.outer_rel_location),
            data_distribution: Some(cmd.data_distribution),
            zipf_exponent: if cmd.data_distribution == ArgDataDistribution::Zipf {
                cmd.zipf_exponent
            } else {
                None
            },
            join_selectivity: Some(cmd.selectivity as f64 / 100.0),
            ..self.clone()
        };

        Ok(dp)
    }
}
