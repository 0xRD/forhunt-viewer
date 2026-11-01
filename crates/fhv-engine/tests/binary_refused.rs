use fhv_engine::{OpenOptions, Table};

fn open_bytes(name: &str, bytes: &[u8]) -> Result<Table, fhv_engine::Error> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    Table::open(&path, opts)
}

#[test]
fn hive_log_and_binary_refused() {
    let mut log = b"regf".to_vec();
    log.extend(std::iter::repeat_n(0u8, 4096));
    let err = open_bytes("NTUSER.DAT.LOG1", &log).err().unwrap();
    assert!(
        err.to_string().contains("not a delimited text file"),
        "{err}"
    );
    let mut nul = b"a,b\n1,2\n".to_vec();
    nul.extend(std::iter::repeat_n(0u8, 64));
    assert!(open_bytes("x.bin", &nul).is_err());
}

#[test]
fn text_still_opens() {
    assert!(open_bytes("a.csv", b"a,b\n1,2\n").is_ok());
    assert!(open_bytes("mz.csv", b"MZ,DIRT\n1,2\n").is_ok());
    assert!(open_bytes("dirt.csv", b"DIRT,size\n1,2\n").is_ok());
    let utf16: Vec<u8> = "a,b\n1,2\n"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    assert!(open_bytes("u.csv", &utf16).is_ok());
}
