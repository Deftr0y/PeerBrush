use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env::var_os("PB_FLAGS_ROOT").unwrap());
    let target = env::var("PB_FLAGS_TARGET").unwrap();
    let python = env::var("PB_FLAGS_PYTHON").unwrap();
    let launcher = root.join("launcher.py");
    for (name, cpp) in [("c", false), ("cpp", true)] {
        let mut build = cc::Build::new();
        build
            .cargo_metadata(false)
            .host(&target)
            .target(&target)
            .opt_level(0)
            .debug(false)
            .warnings(false)
            .cpp(cpp)
            .out_dir(&root);
        let tool = build.get_compiler();
        let mut bytes = Vec::new();
        for argument in tool.args() {
            let value = argument.to_str().unwrap().as_bytes();
            bytes.extend_from_slice(&(u32::try_from(value.len()).unwrap()).to_be_bytes());
            bytes.extend_from_slice(value);
        }
        fs::write(root.join(format!("cc-{name}.args")), bytes).unwrap();
        let mut command = tool.to_command();
        if tool.is_like_msvc() {
            command.arg("/EP");
        } else {
            command.args(["-E", "-P"]);
        }
        let output = command
            .arg(root.join(format!("native.{name}")))
            .output()
            .expect("native preprocessing could not start");
        assert!(output.status.success(), "native preprocessing failed");
        fs::write(root.join(format!("preprocessed-{name}.txt")), output.stdout).unwrap();
    }
    let mut config = cmake::Config::new(&root);
    config
        .host(&target)
        .target(&target)
        .profile("Release")
        .generator("Ninja")
        .out_dir(root.join("cmake-output"))
        .define(
            "CMAKE_MAKE_PROGRAM",
            env::var_os("CMAKE_MAKE_PROGRAM").unwrap(),
        )
        .define(
            "CMAKE_C_COMPILER_LAUNCHER",
            format!("{python};{}", launcher.display()),
        )
        .define(
            "CMAKE_CXX_COMPILER_LAUNCHER",
            format!("{python};{}", launcher.display()),
        )
        .build_target("flags_probe");
    config.build();
}
