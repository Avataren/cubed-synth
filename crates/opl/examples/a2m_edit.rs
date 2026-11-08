//! Applies a list of edit operations (a JSON array, as `A2mDoc::apply` takes
//! them) to an Adlib Tracker II module and writes the result.
//!
//! cargo run -p cubed-opl --release --example a2m_edit -- in.a2m ops.json out.a2m
//!
//! With `in.a2m` given as `new` (OPL2) or `new3` (OPL3) the edit starts from an
//! empty song.

use cubed_opl::edit::A2mDoc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, ops, output] = &args[..] else {
        panic!("usage: a2m_edit <in.a2m | new | new3> <ops.json> <out.a2m>");
    };
    let mut doc = match input.as_str() {
        "new" => A2mDoc::new(false),
        "new3" => A2mDoc::new(true),
        path => A2mDoc::from_bytes(&std::fs::read(path).unwrap()).unwrap_or_else(|e| panic!("{e}")),
    };
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
        .to_bytes()
        .unwrap_or_else(|e| panic!("cannot save: {e}"));
    std::fs::write(output, bytes).unwrap();
    eprintln!(
        "{} operations, {} undo steps",
        ops.as_array().unwrap().len(),
        doc.info()["undo_steps"]
    );
}
