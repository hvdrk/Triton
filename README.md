Triton for PCIe
==============

## Changes


Please check following documents

 * [Changes](./docs/PCIe.pdf)
 * [Build Guide](./docs/BuildGuide.pdf)


## Commands

to run triton code, use following command

```
export REL_SIZE=128
export EXECUTION_STRATEGY="GpuTritonJoinTwoPass"
export HASHING_SCHEME="BucketChaining"
export HIST_ALOGRITHM_1="GpuChunked"
export HIST_ALOGRITHM_2="GpuContiguous"
export PARTITION_ALOGRITHM_1="GpuHSSWWCv4"
export PARTITION_ALOGRITHM_2="GpuHSSWWCv4"
export PARTITION_MEMTYPE="Numapinned"
export REPEAT=2
cargo run                                          \
  --release                                        \
  --package radix-join                             \
  --                                               \
  --execution-strategy "$EXECUTION_STRATEGY"        \
  --hashing-scheme "$HASHING_SCHEME"                  \
  --histogram-algorithm "$HIST_ALOGRITHM_1"             \
  --histogram-algorithm-2nd "$HIST_ALOGRITHM_2"             \
  --partition-algorithm "$PARTITION_ALOGRITHM_1"                   \
  --partition-algorithm-2nd "$PARTITION_ALOGRITHM_2"                \
  --radix-bits 7,11                                \
  --page-type Huge2MB                      \
  --dmem-buffer-size 8                             \
  --threads 24                                     \
  --device-id 2 \
  --rel-mem-type Numapinned                    \
  --inner-rel-location 0                           \
  --outer-rel-location 0                           \
  --partitions-mem-type "$PARTITION_MEMTYPE"             \
  --partitions-location 0                          \
  --partitions-location 0                          \
  --data-set Custom                                \
  --inner-rel-tuples `bc <<< "$REL_SIZE * 10^6"`         \
  --outer-rel-tuples `bc <<< "$REL_SIZE * 10^6"`         \
  --tuple-bytes Bytes16                            \
  --repeat "$REPEAT"                                      \
  --csv ${EXECUTION_STRATEGY}_${HASHING_SCHEME}_${HIST_ALOGRITHM_1}_${HIST_ALOGRITHM_2}_${PARTITION_ALOGRITHM_1}_${PARTITION_ALOGRITHM_2}_${PARTITION_MEMTYPE}_x${REPEAT}_${REL_SIZE}M.csv

```
The valid range of each radix bits is determined by dmem-buffer-size.

In current system, each radix-bits can not exceed 12 when 8 bytes.

Total radix bits should large enough to accomodate shared memory size.

## What is Project Triton?

Project Triton is a research project that aims to scale data management on GPUs
to a large data size by utilizing fast interconnects. Fast interconnects such
as NVLink 2.0 provide GPUs with high-bandwidth, cache-coherent access to main
memory. Thus, we want to unlock higher DBMS query performance with this new
class of hardware!

In this project, we rethink database design to take full advantage of fast
interconnects. GPUs can store only several gigabytes of data in their on-board
memory, while current interconnect technologies (e.g., PCI Express) are too
slow to transfer data ad hoc to the GPU. In contrast, CPUs are able to access
terabytes of data in main memory. Thus, GPU-based systems run into a data
transfer bottleneck.

Fast interconnects provide a path towards querying large data volumes
"out-of-core" in main memory. The Triton Project explores the ways in which
database management systems can take advantage of fast interconnects to achieve
a high data volume scalability.

## Guides

We provide a series of guides to setup Project Triton on your hardware, and on
how we tuned our code for IBM POWER9 CPUs and Nvidia Volta GPUs:

 * [Setup Guide](./guides/setup.md)

 * [Benchmarking Guide](./guides/benchmarking.md)

 * [Problems when getting started](./guides/problems.md)

 * [Huge pages tuning](./guides/huge_pages.md)

 * [NUMA in the context of fast interconnects](./guides/numa.md)

 * [POWER9 Microarchitecture Tuning](./guides/power9.md)

## Code Structure

Project Triton provides the following applications and libraries:

 * [`datagen`](https://tu-berlin-dima.github.io/fast-interconnects/datagen/index.html)
   is a application and library to generate data with data distributions. It is
   used as a library by `data-store` and `tpch-bench`.
 * [`data-store`](https://tu-berlin-dima.github.io/fast-interconnects/data_store/index.html)
   is a library for generating relational data sets. It is used by `hashjoin`
   and `radix-join`.
 * [`hashjoin`](https://tu-berlin-dima.github.io/fast-interconnects/hashjoin/index.html)
   is an application to execute and benchmark hash joins on CPUs and GPUs.
 * [`microbench`](https://tu-berlin-dima.github.io/fast-interconnects/microbench/index.html)
   is a collection of microbenchmarks for CPUs, GPUs, and GPU interconnects.
 * [`numa-gpu`](https://tu-berlin-dima.github.io/fast-interconnects/numa_gpu/index.html)
   is a library with abstractions and tools to program GPUs with and without
   NVLink.
 * [`radix-join`](https://tu-berlin-dima.github.io/fast-interconnects/radix_join/index.html)
   is an application to execute and benchmark radix joins on CPUs and GPUs. The
   distinction from `hashjoin` enables a specialized API for radix joins.
 * [`sql-ops`](https://tu-berlin-dima.github.io/fast-interconnects/sql_ops/index.html)
   is a library that implements SQL operators. These are used by `hashjoin` and
   `radix-join`.
 * [`tpch-bench`](https://tu-berlin-dima.github.io/fast-interconnects/tpch_bench/index.html)
   is an application to execute and benchmark TPC-H on CPUs and GPUs.
   Currently, Query 6 is implemented.

Code documentation is available on GitHub Pages, and linked in the above list.
You can also build it yourself by running:
```sh
cargo doc --document-private-items --no-deps --open
```

## Research

We've published our results from the Triton Project as academic papers:

 * [Lutz et al., *Pump Up the Volume: Processing Large Data on GPUs with Fast
   Interconnects*, SIGMOD 2020](https://doi.org/10.1145/3318464.3389705)

 * [Lutz et al., *Triton Join: Efficiently Scaling to a Large Join State on
   GPUs with Fast Interconnects*, SIGMOD
   2022](https://doi.org/10.1145/3514221.3517911)

To cite our works, add these BibTeX snippets to your bibliography:

```
@InProceedings{lutz:sigmod:2020,
  author        = {Clemens Lutz and Sebastian Bre{\ss} and Steffen Zeuch and
                  Tilmann Rabl and Volker Markl},
  title         = {Pump up the volume: {Processing} large data on {GPUs} with
                  fast interconnects},
  booktitle     = {{SIGMOD}},
  pages         = {1633--1649},
  publisher     = {{ACM}},
  address       = {New York, NY, USA},
  year          = {2020},
  doi           = {10.1145/3318464.3389705}
}

@InProceedings{lutz:sigmod:2022,
  author        = {Clemens Lutz and Sebastian Bre{\ss} and Steffen Zeuch and
                  Tilmann Rabl and Volker Markl},
  title         = {Triton join: {Efficiently} scaling to a large join state
                  on {GPUs} with fast interconnects},
  booktitle     = {{SIGMOD}},
  pages         = {1017–1032},
  publisher     = {{ACM}},
  address       = {New York, NY, USA},
  year          = {2022},
  doi           = {10.1145/3514221.3517911}
}
```
