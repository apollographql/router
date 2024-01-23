use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use anyhow::ensure;
use anyhow::Context;
use anyhow::Result;
use base64::engine::general_purpose::STANDARD as Base64;
use base64::Engine;
use xtask::*;

const ENTITLEMENTS: &str = "macos-entitlements.plist";

#[derive(Debug, clap::Parser)]
pub struct PackageMacos {
    /// Keychain keychain_password.
    #[clap(long)]
    keychain_password: String,

    /// Certificate bundle in base64.
    #[clap(long)]
    cert_bundle_base64: String,

    /// Certificate bundle keychain_password.
    #[clap(long)]
    cert_bundle_password: String,

    /// Apple team ID.
    #[clap(long)]
    apple_team_id: String,

    /// Apple username.
    #[clap(long)]
    apple_username: String,

    /// Notarization password.
    #[clap(long)]
    notarization_password: String,
}

impl PackageMacos {
    pub fn run(&self, release_path: impl AsRef<Path>) -> Result<()> {
        let release_path = release_path.as_ref();
        let temp = tempfile::tempdir().context("could not create temporary directory")?;

        eprintln!("Temporary directory created at: {}", temp.path().display());

        let keychain_name = temp.path().file_name().unwrap().to_str().unwrap();

        let entitlements = PKG_PROJECT_ROOT.join(ENTITLEMENTS);
        ensure!(entitlements.exists(), "could not find entitlements file");

        eprintln!("Creating keychain...");
        ensure!(
            Command::new("security")
                .args(["create-keychain", "-p"])
                .arg(&self.keychain_password)
                .arg(keychain_name)
                .status()
                .context("could not start command security")?
                .success(),
            "command exited with error",
        );

        eprintln!("Removing relock timeout on keychain...");
        ensure!(
            Command::new("security")
                .arg("set-keychain-settings")
                .arg(keychain_name)
                .status()
                .context("could not start command security")?
                .success(),
            "command exited with error",
        );

        eprintln!("Decoding certificate bundle...");
        let certificate_path = temp.path().join("certificate.p12");
        std::fs::write(
            &certificate_path,
            Base64
                .decode(&self.cert_bundle_base64)
                .context("could not decode base64 encoded certificate bundle")?,
        )
        .context("could not write decoded certificate to file")?;

        eprintln!("Importing codesigning certificate to build keychain...");
        ensure!(
            Command::new("security")
                .arg("import")
                .arg(&certificate_path)
                .arg("-k")
                .arg(keychain_name)
                .arg("-P")
                .arg(&self.cert_bundle_password)
                .arg("-T")
                .arg(which::which("codesign").context("could not find codesign")?)
                .status()
                .context("could not start command security")?
                .success(),
            "command exited with error",
        );

        eprintln!("Adding the codesign tool to the security partition-list...");
        ensure!(
            Command::new("security")
                .args([
                    "set-key-partition-list",
                    "-S",
                    "apple-tool:,apple:,codesign:",
                    "-s",
                    "-k"
                ])
                .arg(&self.keychain_password)
                .arg(keychain_name)
                .status()
                .context("could not start command security")?
                .success(),
            "command exited with error",
        );

        eprintln!("Setting default keychain...");
        ensure!(
            Command::new("security")
                .args(["default-keychain", "-d", "user", "-s"])
                .arg(keychain_name)
                .status()
                .context("could not start command security")?
                .success(),
            "command exited with error",
        );

        eprintln!("Unlocking keychain...");
        ensure!(
            Command::new("security")
                .args(["unlock-keychain", "-p"])
                .arg(&self.keychain_password)
                .arg(keychain_name)
                .status()
                .context("could not start command security")?
                .success(),
            "command exited with error",
        );

        eprintln!("Verifying keychain is set up correctly...");
        let output = Command::new("security")
            .args(["find-identity", "-v", "-p", "codesigning"])
            .stderr(Stdio::inherit())
            .output()
            .context("could not start command security")?;
        let _ = std::io::stdout().write(&output.stdout);
        ensure!(output.status.success(), "command exited with error",);
        ensure!(
            !String::from_utf8_lossy(&output.stdout).contains("0 valid identities found"),
            "no valid identities found",
        );

        eprintln!("Signing code (step 1)...");
        ensure!(
            Command::new("codesign")
                .arg("--sign")
                .arg(&self.apple_team_id)
                .args(["--options", "runtime", "--entitlements"])
                .arg(&entitlements)
                .args(["--force", "--timestamp"])
                .arg(release_path)
                .arg("-v")
                .status()
                .context("could not start command codesign")?
                .success(),
            "command exited with error",
        );

        eprintln!("Signing code (step 2)...");
        ensure!(
            Command::new("codesign")
                .args(["-vvv", "--deep", "--strict"])
                .arg(release_path)
                .status()
                .context("could not start command codesign")?
                .success(),
            "command exited with error",
        );

        eprintln!("Zipping dist...");
        let dist_zip = temp
            .path()
            .join(format!("{}-{}.zip", PKG_PROJECT_NAME, *PKG_VERSION));
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(
            std::fs::File::create(&dist_zip).context("could not create file")?,
        ));
        let options = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o755);
        let path = Path::new("dist").join(RELEASE_BIN);
        eprintln!("Adding {} as {}...", release_path.display(), path.display());
        zip.start_file(path.to_str().unwrap(), options)?;
        std::io::copy(
            &mut std::io::BufReader::new(
                std::fs::File::open(release_path).context("could not open file")?,
            ),
            &mut zip,
        )?;
        zip.finish()?;

        let dist_zip = dist_zip
            .to_str()
            .unwrap_or_else(|| panic!("'{} is not valid UTF-8", dist_zip.display()));

        eprintln!("Beginning notarization process...");
        let output = Command::new("xcrun")
            .args([
                "notarytool",
                "submit",
                dist_zip,
                "--apple-id",
                &self.apple_username,
                "--team-id",
                &self.apple_team_id,
                "--password",
                &self.notarization_password,
                "--wait",
                "--timeout",
                "20m",
            ])
            .stderr(Stdio::inherit())
            .output()
            .context("could not start command xcrun")?;
        let _ = std::io::stdout().write(&output.stdout);
        ensure!(output.status.success(), "command exited with error",);

        eprintln!("Notarization successful");

        Ok(())
    }
}
