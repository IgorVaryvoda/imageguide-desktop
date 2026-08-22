use cargo_packager_updater::{Config, check_update};

const ENDPOINT: &str =
    "https://github.com/IgorVaryvoda/imageguide-desktop/releases/latest/download/latest.json";

pub fn install_if_available() {
    std::thread::spawn(|| {
        let config = Config {
            endpoints: vec![ENDPOINT.parse().expect("the update URL is valid")],
            pubkey: include_str!("../assets/updater.pub").into(),
            ..Default::default()
        };
        let version = env!("CARGO_PKG_VERSION")
            .parse()
            .expect("the package version is semver");

        match check_update(version, config) {
            Ok(Some(update)) => match update.download_and_install() {
                Ok(()) => eprintln!("imageguide: installed update; restart to use it"),
                Err(error) => eprintln!("imageguide: could not install update: {error}"),
            },
            Ok(None) => {}
            Err(error) => eprintln!("imageguide: could not check for updates: {error}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_endpoint_is_https() {
        assert!(ENDPOINT.starts_with("https://"));
    }
}
