// Copyright (c) 2026 Kirky.X🌠
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// proto/sdforge.v1.proto 是 sdforge 仓库同名文件的逐字节副本：NexusFlow 的
// gRPC 门面即 sdforge 的 sdforge.v1.SdForgeService（无自有 proto），客户端只
// 消费这份 wire 契约、不引网关内部 crate。门面消息/服务定义变更时需从
// sdforge 重新同步本副本。
//
// 构建前置：protoc 须在 PATH（与 sdforge grpc feature 的构建前置一致）。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")?;
    let proto_files = &[std::path::Path::new(&manifest_dir).join("proto/sdforge.v1.proto")];
    let proto_includes = &[std::path::Path::new(&manifest_dir).join("proto")];
    println!("cargo:rerun-if-changed={}", proto_files[0].display());

    // 只生成客户端 stub：服务器端由网关进程承担（sdforge 以 build_server(true)
    // 自行编译），样例多生成 server 代码只会引入无消费者的编译负担。
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(proto_files, proto_includes)?;

    Ok(())
}
