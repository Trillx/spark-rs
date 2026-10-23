fn main() {
    // Link the VC++ runtime statically, as tauri-build does for spark-desktop,
    // so spark-cli.exe runs on Windows machines without the VC++ Redistributable.
    static_vcruntime::metabuild();
}
