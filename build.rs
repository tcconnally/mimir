fn main() {
    #[cfg(feature = "grpc")]
    {
        tonic_build::configure()
            .build_server(true)
            .build_client(false)
            .compile_protos(
                &["proto/mimir/v1/mimir.proto"],
                &["proto"],
            )
            .expect("failed to compile mimir proto");
    }
}
