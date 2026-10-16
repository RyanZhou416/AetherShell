//! Returned paths remain usable without changing the filesystem paths used by operations.
use aethershell::{builtins::call_with_input, modules::env_with_modules, value::Value};

fn call(name: &str, args: Vec<Value>) -> Value {
    call_with_input(name, args, None, &mut env_with_modules()).unwrap()
}
fn text(path: &std::path::Path) -> Value {
    Value::Str(path.to_str().unwrap().to_string())
}

#[test]
fn ordinary_listing_realpath_and_read_results_share_a_usable_path() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("中文.txt");
    std::fs::write(&file, "hello\r\n😀").unwrap();
    let expected = aethershell::path_display::display_path(&file.canonicalize().unwrap());
    let Value::Array(rows) = call("ls", vec![text(root.path())]) else {
        panic!("array expected")
    };
    let Value::Record(row) = &rows[0] else {
        panic!("record expected")
    };
    assert_eq!(row["path"], Value::Str(expected.clone()));
    assert!(std::path::Path::new(&expected).is_file());
    assert_eq!(
        call("fs_realpath", vec![text(&file)]),
        Value::Str(expected.clone())
    );
    let Value::Record(read) = call("file_read_text", vec![text(&file)]) else {
        panic!("record expected")
    };
    assert_eq!(read["path"], Value::Str(expected));
    assert_eq!(read["text"], Value::Str("hello\r\n😀".to_string()));
}

#[cfg(windows)]
#[test]
fn a_verbatim_only_file_keeps_its_exact_spelling() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().canonicalize().unwrap().join("trailing.");
    std::fs::write(&file, b"verbatim").unwrap();
    let expected = file.to_str().unwrap();
    assert!(expected.starts_with(r"\\?\"));
    assert_eq!(aethershell::path_display::display_path(&file), expected);
    let Value::Array(rows) = call("ls", vec![text(root.path())]) else {
        panic!("array expected")
    };
    let Value::Record(row) = &rows[0] else {
        panic!("record expected")
    };
    assert_eq!(row["path"], Value::Str(expected.to_string()));
    std::fs::remove_file(&file).unwrap();
}
