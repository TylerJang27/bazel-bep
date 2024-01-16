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
    tonic_build::configure()
        .build_client(false)
        .compile(&protos, &["proto/"])?;
    Ok(())
}
