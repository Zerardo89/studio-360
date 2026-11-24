# Build di Studio 360

Con Rust stable per `x86_64-pc-windows-gnu` già installato, apri PowerShell in `app/` ed esegui `cargo build`, `cargo build --release` o `cargo test`.
Cargo legge `.cargo/config.toml` e usa lo shim incluso in `tools/bin/` con i linker della toolchain.
Per verificare la schermata iniziale, esegui `cargo run -- --tab home --screenshot shots/verify.png`.

Per ricompilare lo shim quando ne aggiorni il sorgente:

```powershell
rustc --edition=2021 -C opt-level=2 -C strip=symbols -C linker=rust-lld.exe -C linker-flavor=ld.lld -C link-self-contained=yes tools/dlltool-shim/main.rs -o tools/bin/dlltool.exe
```
