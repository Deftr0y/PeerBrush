use super::*;
use crate::file_safety::wide_absolute_file_path;

#[test]
fn native_paths_preserve_unicode_unc_and_already_extended_names() {
    let decode = |path: &Path| {
        let wide = wide_absolute_file_path(path).unwrap();
        assert_eq!(wide.last(), Some(&0));
        assert!(!wide[..wide.len() - 1].contains(&0));
        String::from_utf16(&wide[..wide.len() - 1]).unwrap()
    };
    assert_eq!(
        decode(Path::new(r"C:\folder\作品.txt")),
        r"\\?\C:\folder\作品.txt"
    );
    assert_eq!(
        decode(Path::new(r"\\server\share\作品.txt")),
        r"\\?\UNC\server\share\作品.txt"
    );
    assert_eq!(
        decode(Path::new(r"\\?\C:\folder\作品.txt")),
        r"\\?\C:\folder\作品.txt"
    );
    assert_eq!(
        decode(Path::new(r"\\?\UNC\server\share\作品.txt")),
        r"\\?\UNC\server\share\作品.txt"
    );
    assert!(decode(Path::new("relative-file.tmp")).starts_with(r"\\?\"));
    assert!(wide_absolute_file_path(Path::new("file\0name")).is_err());
}

#[test]
fn long_path_atomic_replacement_and_late_refusal_preserve_destination() {
    let owner = std::env::temp_dir().join(format!("pb-atomic-long-{}", engine::id()));
    let directory = owner.join("a".repeat(100)).join("b".repeat(100));
    fs::create_dir_all(&directory).unwrap();
    let owned = fs::canonicalize(&owner).unwrap();
    assert_eq!(
        owned.parent().unwrap(),
        fs::canonicalize(std::env::temp_dir()).unwrap()
    );
    let path = directory.join("native-state.json");
    assert!(path.as_os_str().len() > 260);
    atomic_write(&path, b"first complete bytes").unwrap();
    atomic_write(&path, b"second complete bytes").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"second complete bytes");
    let error = atomic_write_checked(&path, b"unpublished candidate", |temporary| {
        assert_eq!(fs::read(temporary).unwrap(), b"unpublished candidate");
        Err("The captured source changed".into())
    })
    .unwrap_err();
    assert_eq!(error, "The captured source changed");
    assert_eq!(fs::read(&path).unwrap(), b"second complete bytes");
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    assert_eq!(fs::canonicalize(&owner).unwrap(), owned);
    fs::remove_dir_all(owned).unwrap();
}
