use callimachus_adapter_code::chunker::{ChunkOptions, chunk_directory};
use callimachus_adapter_code::extractor::extract_structure;
#[tokio::test]
async fn prof() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..2000u32 {
        std::fs::write(
            dir.path().join(format!("F{i}.c")),
            format!("void FUN_{i:08x}(void) {{ DAT_{i:08x} = FUN_other(); }}\n"),
        )
        .unwrap();
    }
    let t = std::time::Instant::now();
    let chunks = chunk_directory(dir.path(), "c", &ChunkOptions::default())
        .await
        .unwrap();
    println!("chunk {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let lang = callimachus_adapter_code::languages::for_extension("c").unwrap();
    for c in &chunks {
        extract_structure(c, lang).unwrap();
    }
    println!("extract {:?}", t.elapsed());
}
