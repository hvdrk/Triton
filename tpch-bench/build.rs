/*
 * This Source Code Form is subject to the terms of the Mozilla Public License,
 * v. 2.0. If a copy of the MPL was not distributed with this file, You can
 * obtain one at http://mozilla.org/MPL/2.0/.
 *
 *
 * Copyright 2020 German Research Center for Artificial Intelligence (DFKI)
 * Author: Clemens Lutz <clemens.lutz@dfki.de>
 */

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();

    // Add CUDA utils
    let cuda_lib_file = format!("{}/cudautils.fatbin", out_dir);
    let cuda_files = vec!["cudautils/queries.cu"];
    let nvcc_build_args = vec![
        "-ccbin",
        "/usr/bin/g++-7",
        "--device-c",
        "-std=c++11",
        "--output-directory",
        &out_dir,
    ];
    let nvcc_link_args = vec!["--device-link", "-fatbin", "--output-file", &cuda_lib_file];

    // For gencodes, see: http://arnon.dk/matching-sm-architectures-arch-and-gencode-for-various-nvidia-cards/
    let gpu_archs = vec![
        "-gencode",
        "arch=compute_50,code=sm_50", // GTX 940M
        "-gencode",
        "arch=compute_52,code=sm_52", // GTX 980
        "-gencode",
        "arch=compute_53,code=sm_53", // Jetson Nano
        "-gencode",
        "arch=compute_61,code=sm_61", // GTX 1080
        "-gencode",
        "arch=compute_70,code=sm_70", // Tesla V100
    ];

    let output = Command::new("nvcc")
        .args(cuda_files.as_slice())
        .args(nvcc_build_args.as_slice())
        .args(gpu_archs.as_slice())
        .output()
        .expect("Couldn't execute nvcc");

    if !output.status.success() {
        eprintln!("status: {}", output.status);
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!();
    }

    let cuda_object_files: Vec<_> = cuda_files
        .as_slice()
        .iter()
        .map(|f| {
            let p = Path::new(f);
            let mut obj = PathBuf::new();
            obj.push(&out_dir);
            obj.push(p.file_stem().unwrap());
            obj.set_extension("o");
            obj
        })
        .collect();

    let output = Command::new("nvcc")
        .args(cuda_object_files.as_slice())
        .args(nvcc_link_args.as_slice())
        .args(gpu_archs.as_slice())
        .output()
        .expect("Couldn't execute nvcc");

    if !output.status.success() {
        eprintln!("status: {}", output.status);
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!();
    }

    println!(
        "cargo:rustc-env=CUDAUTILS_PATH={}/cudautils.fatbin",
        out_dir
    );
    println!("cargo:rustc-link-search=native=/opt/cuda/lib64");
    println!("cargo:rustc-link-search=native=/usr/local/cuda/lib64");
    println!("cargo:rustc-link-lib=cudart");

    // Add CPP utils
    cc::Build::new()
        .compiler("gcc-8")
        .cpp(true)
        // Note: -march not supported by GCC-7 on Power9, use -mcpu instead
        .flag("-std=c++11")
        .debug(true)
        .flag_if_supported("-mcpu=native")
        .flag_if_supported("-march=native")
        .flag("-mtune=native")
        // Note: Enables x86 intrinsic translations on POWER9
        // See also "Linux on Power Porting Guide - Vector Intrinsics"
        .define("NO_WARN_X86_INTRINSICS", None)
        .pic(true)
        .file("cpputils/queries.cpp")
        .compile("libcpputils.a");
}
