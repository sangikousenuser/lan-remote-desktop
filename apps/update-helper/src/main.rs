fn run() -> lrd_session::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5
        || args[0] != "--restart"
        || args[1] != "--installed"
        || args[3] != "--wait-pid"
    {
        return lrd_session::fail("updater is started by the application at next launch");
    }
    semver_check(&args[2])?;
    lrd_session::updates::service::restart_helper(&args[2], args[4].parse()?)
}
fn semver_check(version: &str) -> lrd_session::Result<()> {
    if version.len() > 128 {
        return lrd_session::fail("version limit");
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Updater: {e}");
        std::process::exit(1);
    }
}
