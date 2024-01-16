use std::io;

fn main() -> io::Result<()> {
    let protos = std::fs::read_dir("proto")?
        .into_iter()
        .filter(|a| {
            if let Ok(a) = a {
                a.file_type().unwrap().is_file()
            } else {
                false
            }
        })
        .map(|a| a.map(|a| a.path().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    let compiler = tonic_build::configure();
    #[cfg(not(feature = "client"))]
    let compiler = compiler.build_client(false);
    #[cfg(not(feature = "server"))]
    let compiler = compiler.build_server(false);
    compiler.compile(&protos, &["proto/"])?;

    Ok(())
}
