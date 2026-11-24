use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::PathBuf,
    process::{Command, ExitCode, ExitStatus},
};

fn run_rustc(rustc: &OsStr, args: &[OsString]) -> io::Result<ExitStatus> {
    // Cargo compiles dependencies from their own directories. rustc needs
    // an absolute dlltool path when it starts the import-library generator.
    let mut dlltool = OsString::from("dlltool=");
    dlltool.push(env::current_exe()?);
    let args = args.iter().map(|arg| {
        if arg == "dlltool=tools/bin/dlltool.exe" {
            &dlltool
        } else {
            arg
        }
    });
    Command::new(rustc).args(args).status()
}

fn lld_link() -> io::Result<PathBuf> {
    let output = Command::new("rustc.exe")
        .args(["--print", "sysroot"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "rustc --print sysroot: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let sysroot = String::from_utf8(output.stdout).map_err(io::Error::other)?;
    Ok(PathBuf::from(sysroot.trim_end_matches(['\r', '\n']))
        .join("lib/rustlib/x86_64-pc-windows-gnu/bin/gcc-ld/lld-link.exe"))
}

fn run_dlltool(args: &[OsString]) -> io::Result<ExitStatus> {
    let option = |name: &str| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].as_os_str())
            .ok_or_else(|| io::Error::other(format!("missing dlltool option {name}")))
    };
    let definition = PathBuf::from(option("-d")?);
    let dll = option("-D")?;
    let output = option("-l")?;
    let linker = lld_link()?;

    let mut contents = fs::read_to_string(&definition)?;
    if !contents
        .lines()
        .any(|line| line.trim_start().starts_with("LIBRARY "))
    {
        contents = format!("LIBRARY {}\n{contents}", dll.to_string_lossy());
    }
    let prepared = definition.with_extension("llvm.def");
    fs::write(&prepared, contents)?;

    let mut def_arg = OsString::from("/def:");
    def_arg.push(&prepared);
    let mut out_arg = OsString::from("/out:");
    out_arg.push(output);

    let status = Command::new(linker)
        .args(["/lib", "/machine:x64"])
        .args([def_arg, out_arg])
        .status();
    let cleanup = fs::remove_file(prepared);
    let status = status?;
    if status.success() {
        cleanup?;
    }
    Ok(status)
}

fn main() -> ExitCode {
    // Cargo passes rustc as the first argument. rustc starts dlltool with options.
    let args: Vec<_> = env::args_os().skip(1).collect();
    let status = match args.split_first() {
        Some((rustc, rest)) if !rustc.to_string_lossy().starts_with('-') => run_rustc(rustc, rest),
        _ => run_dlltool(&args),
    };
    match status {
        Ok(status) => ExitCode::from(
            status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1),
        ),
        Err(error) => {
            eprintln!("dlltool shim: {error}");
            ExitCode::FAILURE
        }
    }
}
