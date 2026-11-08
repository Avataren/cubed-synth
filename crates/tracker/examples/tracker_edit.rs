//! Applies a list of edit operations (a JSON array, as `ModuleDoc::apply`
//! takes them) to a module file and writes the result in the same format.
//!
//! cargo run -p cubed-tracker --release --example tracker_edit -- in.xm ops.json out.xm

use cubed_tracker::edit::ModuleDoc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, ops, output] = &args[..] else {
        panic!("usage: tracker_edit <module> <ops.json> <out>");
    };
    let mut doc = ModuleDoc::from_bytes(&std::fs::read(input).unwrap()).unwrap();
    let ops: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(ops).unwrap()).unwrap();
    for (i, op) in ops
        .as_array()
        .expect("ops.json is an array")
        .iter()
        .enumerate()
    {
        if let Err(e) = doc.apply(op) {
            panic!("operation {i} ({op}): {e}");
        }
    }
    let bytes = doc
        .to_bytes(doc.format())
        .unwrap_or_else(|e| panic!("cannot save: {e}"));
    std::fs::write(output, bytes).unwrap();
    eprintln!(
        "{} operations, {} undo steps",
        ops.as_array().unwrap().len(),
        doc.info()["undo_steps"]
    );
}
