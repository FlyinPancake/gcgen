mod input {
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
    };

    use garde::Validate;
    use schemars::JsonSchema;
    use serde::Deserialize;

    use crate::error::GcGenError;

    // The guard may be overkill, but it ensures that the alias is not empty and only contains alphanumeric characters.
    #[derive(
        Debug, Deserialize, JsonSchema, Validate, Clone, PartialEq, Eq, PartialOrd, Ord, Hash,
    )]
    #[serde(transparent)]
    pub struct GitAliasName(
        #[garde(length(min = 1), alphanumeric)]
        #[schemars(length(min = 1), pattern("^[a-zA-Z0-9]+$"))]
        String,
    );

    impl GitAliasName {
        pub fn as_str(&self) -> &str {
            &self.0
        }
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    pub struct GitConfig {
        pub user_name: String,
        pub user_email: String,
        #[serde(default)]
        pub gpg: GpgConfig,
        #[serde(default)]
        pub aliases: BTreeMap<GitAliasName, String>,
        pub ssh: Option<SshConfig>,
        /// Register the git-lfs filter, like `git lfs install` does.
        #[serde(default)]
        pub lfs: bool,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    pub struct SshConfig {
        /// Key to authenticate with. A public key file selects the matching key from ssh-agent.
        pub identity_file: PathBuf,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum SshKeySource {
        Key(String),
        File(PathBuf),
    }

    #[derive(Debug, Deserialize, JsonSchema, Default)]
    #[serde(rename_all = "snake_case")]
    pub enum GpgConfig {
        #[default]
        Off,
        GpgKeyId {
            gpg_key_id: String,
        },
        SshKey(SshKeySource),
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    pub struct GitProfile {
        pub path: PathBuf,
        #[serde(flatten)]
        pub config: GitConfig,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    pub struct Input {
        pub default: GitConfig,
        #[serde(default)]
        pub profiles: BTreeMap<String, GitProfile>,
    }

    impl Input {
        pub fn from_file_path(path: &Path) -> Result<Self, GcGenError> {
            config::Config::builder()
                .add_source(config::File::from(path))
                .build()?
                .try_deserialize()
                .map_err(Into::into)
        }
    }
}

mod render {
    use std::borrow::Cow;

    use gix_config::File;

    use crate::{
        error::GcGenError,
        input::{GitConfig, GpgConfig, Input, SshKeySource},
    };

    impl GitConfig {
        fn write_into(&self, file: &mut File) -> Result<(), GcGenError> {
            let mut user = file.new_section("user", None)?;
            push(&mut user, "name", &self.user_name)?;
            push(&mut user, "email", &self.user_email)?;

            let signing = match &self.gpg {
                GpgConfig::Off => None,
                GpgConfig::GpgKeyId { gpg_key_id } => {
                    Some(("openpgp", Cow::Borrowed(gpg_key_id.as_str())))
                }
                // A literal key needs the `key::` prefix, otherwise git treats it as a path.
                GpgConfig::SshKey(SshKeySource::Key(key)) => {
                    Some(("ssh", Cow::Owned(format!("key::{key}"))))
                }
                GpgConfig::SshKey(SshKeySource::File(path)) => {
                    Some(("ssh", path.to_string_lossy()))
                }
            };
            if let Some((format, key)) = signing {
                push(&mut user, "signingkey", &key)?;
                let mut gpg = file.new_section("gpg", None)?;
                push(&mut gpg, "format", format)?;
                for section in ["commit", "tag"] {
                    let mut section = file.new_section(section, None)?;
                    push(&mut section, "gpgsign", "true")?;
                }
            }

            if let Some(ssh) = &self.ssh {
                let mut core = file.new_section("core", None)?;
                // Left unquoted so the shell git runs this through still expands `~`.
                // IdentitiesOnly stops ssh from offering the agent's other keys first.
                let command = format!(
                    "ssh -i {} -o IdentitiesOnly=yes",
                    ssh.identity_file.to_string_lossy()
                );
                push(&mut core, "sshCommand", &command)?;
            }

            if self.lfs {
                let mut filter = file.new_section("filter", "lfs")?;
                push(&mut filter, "clean", "git-lfs clean -- %f")?;
                push(&mut filter, "smudge", "git-lfs smudge -- %f")?;
                push(&mut filter, "process", "git-lfs filter-process")?;
                push(&mut filter, "required", "true")?;
            }

            if !self.aliases.is_empty() {
                let mut alias = file.new_section("alias", None)?;
                for (name, command) in &self.aliases {
                    push(&mut alias, name.as_str(), command)?;
                }
            }
            Ok(())
        }
    }

    /// A rendered config file, named relative to the directory it belongs in.
    pub struct RenderedFile {
        pub file_name: String,
        pub config: File,
    }

    impl Input {
        /// Renders `.gitconfig` followed by one `.gitconfig-<profile>` per profile.
        pub fn render(&self) -> Result<Vec<RenderedFile>, GcGenError> {
            let mut main = File::default();
            self.default.write_into(&mut main)?;

            let mut files = Vec::with_capacity(self.profiles.len() + 1);
            for (name, profile) in &self.profiles {
                if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
                    return Err(GcGenError::InvalidProfileName(name.clone()));
                }
                let file_name = format!(".gitconfig-{name}");

                // Trailing slash makes `gitdir:` match everything below the directory.
                let gitdir = format!(
                    "gitdir:{}/",
                    profile.path.to_string_lossy().trim_end_matches('/')
                );
                let mut include = main.new_section("includeIf", gitdir.as_str())?;
                // Git resolves relative include paths against the including file's directory.
                push(&mut include, "path", &file_name)?;

                let mut config = File::default();
                profile.config.write_into(&mut config)?;
                files.push(RenderedFile { file_name, config });
            }

            files.insert(
                0,
                RenderedFile {
                    file_name: ".gitconfig".to_owned(),
                    config: main,
                },
            );
            Ok(files)
        }
    }

    fn push(
        section: &mut gix_config::file::SectionMut<'_>,
        key: &str,
        value: &str,
    ) -> Result<(), GcGenError> {
        section.push(key, Some(value.into()))?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use crate::Input;

        #[test]
        fn escapes_values() {
            let input: Input = toml_input(
                r##"
                [default]
                user_name = " padded "
                user_email = "a@b"
                aliases.q = '!echo "hi"'
                aliases.l = "!git log # all"
                "##,
            );
            let config = input.render().unwrap()[0].config.to_string();
            assert!(config.contains("\tname = \" padded \""), "{config}");
            assert!(config.contains("\tq = !echo \\\"hi\\\""), "{config}");
            assert!(config.contains("\tl = \"!git log # all\""), "{config}");
        }

        #[test]
        fn profile_ssh_identity() {
            let input: Input = toml_input(
                r#"
                [default]
                user_name = "me"
                user_email = "me@home"
                ssh.identity_file = "~/.ssh/main.pub"

                [profiles.work]
                path = "~/Work"
                user_name = "me"
                user_email = "me@work"
                ssh.identity_file = "~/.ssh/work.pub"
                "#,
            );
            let files = input.render().unwrap();
            let main = files[0].config.to_string();
            let work = files[1].config.to_string();
            assert!(
                main.contains("\tsshCommand = ssh -i ~/.ssh/main.pub -o IdentitiesOnly=yes"),
                "{main}"
            );
            assert!(
                work.contains("\tsshCommand = ssh -i ~/.ssh/work.pub -o IdentitiesOnly=yes"),
                "{work}"
            );
        }

        #[test]
        fn lfs_filter() {
            let input: Input = toml_input(
                r#"
                [default]
                user_name = "me"
                user_email = "me@home"
                lfs = true
                "#,
            );
            let config = input.render().unwrap()[0].config.to_string();
            assert!(config.contains("[filter \"lfs\"]"), "{config}");
            assert!(config.contains("\tclean = git-lfs clean -- %f"), "{config}");
            assert!(config.contains("\tsmudge = git-lfs smudge -- %f"), "{config}");
            assert!(config.contains("\tprocess = git-lfs filter-process"), "{config}");
            assert!(config.contains("\trequired = true"), "{config}");
        }

        fn toml_input(source: &str) -> Input {
            config::Config::builder()
                .add_source(config::File::from_str(source, config::FileFormat::Toml))
                .build()
                .unwrap()
                .try_deserialize()
                .unwrap()
        }
    }
}

mod error {
    #[derive(Debug, thiserror::Error)]
    pub enum GcGenError {
        #[error("Configuration error: {0}")]
        ConfigError(#[from] config::ConfigError),
        #[error("Failed to render git config: {0}")]
        RenderError(#[source] gix_error::Error),
        #[error("Profile name {0:?} can't be used in a file name")]
        InvalidProfileName(String),
    }

    impl From<gix_error::Exn<gix_error::Message>> for GcGenError {
        fn from(err: gix_error::Exn<gix_error::Message>) -> Self {
            Self::RenderError(err.into_error())
        }
    }
}

pub use input::{GitProfile, Input};
pub use render::RenderedFile;
