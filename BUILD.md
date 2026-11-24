# Build di Studio 360

Con Rust stable per `x86_64-pc-windows-gnu` già installato, apri PowerShell in `app/` ed esegui `cargo build`, `cargo build --release` o `cargo test`.
Cargo legge `.cargo/config.toml` e usa lo shim incluso in `tools/bin/` con i linker della toolchain.
Per verificare la schermata iniziale, esegui `cargo run -- --tab home --screenshot shots/verify.png`.

I contenuti online usano `ureq` 3 con le feature `native-tls` e `json`. Le connessioni HTTPS
usano SChannel e l’archivio dei certificati di Windows. Il catalogo usa `serde` e `serde_json`,
mentre le query usano `percent-encoding`. `ab_glyph` 0.2 rasterizza il testo dai byte di
`epaint_default_fonts` 0.37. La feature Windows `Win32_Media_Audio` abilita l’anteprima WASAPI.
Le feature `Win32_Foundation` e `Win32_Security_Cryptography` abilitano il salvataggio della chiave AI con DPAPI.
Alla prima build Cargo deve poter scaricare le nuove dipendenze oppure trovarle nella cache locale.
Per compilare sul target Windows GNU non serve un compilatore C.

`cargo test` esegue le verifiche locali. `cargo test -- --ignored` prova una ricerca Openverse e
un download Twemoji senza usare la cache. I seguenti comandi catturano i pannelli di Modifica.

```powershell
cargo run -- --tab modifica --open shots/sample360.mp4 --auto-edit --panel musica --screenshot shots/music.png
cargo run -- --tab modifica --panel sticker --screenshot shots/sticker.png
cargo run -- --tab modifica --panel testo --screenshot shots/testo.png
```

I video usano Windows Media Foundation e i codec installati in Windows. Su Windows N il supporto video
richiede il Media Feature Pack. Per i filmati HEVC può servire il pacchetto Estensioni video HEVC.
Il test H.264 si salta se la piattaforma video di Windows non è disponibile.

Per generare e verificare il campione MP4 nella cartella di lavoro:

```powershell
$env:STUDIO360_TEST_MP4 = 'shots/sample360.mp4'
cargo test video::tests::h264_mp4_round_trip -- --nocapture
Remove-Item Env:STUDIO360_TEST_MP4
cargo run -- --tab modifica --open shots/sample360.mp4 --screenshot shots/video.png
cargo run -- --tab album --screenshot shots/album.png
```

Per ricompilare lo shim quando ne aggiorni il sorgente:

```powershell
rustc --edition=2021 -C opt-level=2 -C strip=symbols -C linker=rust-lld.exe -C linker-flavor=ld.lld -C link-self-contained=yes tools/dlltool-shim/main.rs -o tools/bin/dlltool.exe
```
