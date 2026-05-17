use std::process;

/// Entry point for the sample project.
fn main() {
    let rec = sample_project::Record {
        id: 1,
        value: "hello".to_string(),
    };

    match sample_project::process_record(&rec) {
        Ok(result) => println!("{result}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}
