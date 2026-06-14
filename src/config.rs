//! Configuration: typed TOML deserialization, file discovery
//! (order frozen in docs/api.md §7), validation, and minimal CLI parsing.

use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::Duration;

/// Plugins known to this build. A `[plugins.<name>]` section for any other
/// name is an operator typo and must fail loudly, not be silently ignored.
pub const KNOWN_PLUGINS: [&str; 9] = [
    "cpu", "diskio", "fs", "load", "mem", "memswap", "network", "system", "uptime",
];

/// Same default port as the Glances REST server.
const DEFAULT_PORT: u16 = 61208;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub server: ServerConfig,
    pub security: SecurityConfig,
    pub collect: CollectConfig,
    pub plugins: HashMap<String, PluginConfig>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    pub bind: IpAddr,
    pub port: u16,
    /// Cleartext password. Discouraged — prefer `password_env` so the
    /// secret never lives in the config file. Mutually exclusive with it.
    pub password: Option<String>,
    /// Name of the environment variable that holds the password. The config
    /// stores only the variable *name*, never the secret. Resolved at load
    /// time into `password`; a missing or empty variable is a startup error.
    pub password_env: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        // Closed by default (ARCHITECTURE.md §7.1): loopback, no password.
        Self {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: DEFAULT_PORT,
            password: None,
            password_env: None,
        }
    }
}

impl ServerConfig {
    /// Replace `password_env` with the secret read from that environment
    /// variable, leaving downstream code to read `password` uniformly. A
    /// missing/empty variable — or both fields set — is a hard error, so a
    /// misconfigured secret never silently degrades to "no auth".
    fn resolve_password(&mut self) -> Result<(), ConfigError> {
        match (&self.password, &self.password_env) {
            (Some(_), Some(_)) => Err(ConfigError::Invalid(
                "set either [server].password or [server].password_env, not both".into(),
            )),
            (_, Some(var)) => {
                let value = std::env::var(var).map_err(|_| {
                    ConfigError::Invalid(format!(
                        "[server].password_env names ${var}, which is not set \
                         (or not valid UTF-8)"
                    ))
                })?;
                if value.is_empty() {
                    return Err(ConfigError::Invalid(format!(
                        "environment variable ${var} (from [server].password_env) is empty"
                    )));
                }
                self.password = Some(value);
                self.password_env = None;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SecurityConfig {
    /// CORS allow-list (§7.3). Empty = CORS fully closed; never a wildcard.
    pub cors_origins: Vec<String>,
    /// Expected `Host` header values (§7.4).
    pub trusted_hosts: Vec<String>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            cors_origins: Vec::new(),
            trusted_hosts: vec!["localhost".into(), "127.0.0.1".into()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CollectConfig {
    /// Default collection period in seconds, overridable per plugin (§3).
    pub refresh: f64,
    /// A plugin's collector stops after this many refresh periods without
    /// a request (§3: idle timeout, default ≈ 5 cycles).
    pub idle_cycles: u32,
    /// How long a waking request waits for the first collection cycle
    /// before answering 503 (§3, §6.2), in seconds.
    pub guard_timeout: f64,
}

impl Default for CollectConfig {
    fn default() -> Self {
        Self {
            refresh: 2.0,
            idle_cycles: 5,
            guard_timeout: 5.0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PluginConfig {
    /// Fine-grained exposure: a disabled plugin is absent from the API.
    pub enabled: Option<bool>,
    /// Per-plugin override of `collect.refresh`, in seconds.
    pub refresh: Option<f64>,
    /// Regex allow-list on the item primary key (collection plugins).
    pub show: Vec<String>,
    /// Regex deny-list on the item primary key (collection plugins).
    pub hide: Vec<String>,
    /// Item primary key -> display alias (collection plugins, e.g. network
    /// interface names). Empty by default; surfaced verbatim in the payload.
    pub alias: HashMap<String, String>,
}

impl Config {
    /// Parse and validate a TOML document, resolving `password_env` against
    /// the environment so downstream code only ever reads `password`.
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        let mut config: Config = toml::from_str(input).map_err(ConfigError::Parse)?;
        config.validate()?;
        config.server.resolve_password()?;
        Ok(config)
    }

    /// Discover (docs/api.md §7) and load the configuration. Returns the
    /// config and the path it came from (`None` = built-in defaults).
    pub fn load(cli_path: Option<PathBuf>) -> Result<(Self, Option<PathBuf>), ConfigError> {
        match discover_path(cli_path)? {
            Some(path) => {
                let text = std::fs::read_to_string(&path)
                    .map_err(|err| ConfigError::Read(path.clone(), err))?;
                Ok((Self::from_toml(&text)?, Some(path)))
            }
            None => Ok((Self::default(), None)),
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !(self.collect.refresh.is_finite() && self.collect.refresh > 0.0) {
            return Err(ConfigError::Invalid(format!(
                "collect.refresh must be a positive number of seconds, got {}",
                self.collect.refresh
            )));
        }
        if !(self.collect.guard_timeout.is_finite() && self.collect.guard_timeout > 0.0) {
            return Err(ConfigError::Invalid(format!(
                "collect.guard_timeout must be a positive number of seconds, got {}",
                self.collect.guard_timeout
            )));
        }
        if self.collect.idle_cycles == 0 {
            return Err(ConfigError::Invalid(
                "collect.idle_cycles must be at least 1".into(),
            ));
        }
        for (name, plugin) in &self.plugins {
            if !KNOWN_PLUGINS.contains(&name.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "unknown plugin section [plugins.{name}] (known plugins: {})",
                    KNOWN_PLUGINS.join(", ")
                )));
            }
            if let Some(refresh) = plugin.refresh
                && !(refresh.is_finite() && refresh > 0.0)
            {
                return Err(ConfigError::Invalid(format!(
                    "plugins.{name}.refresh must be a positive number of seconds, got {refresh}"
                )));
            }
            // show/hide patterns are compiled here once so plugins can
            // assume they are valid — a bad regex fails at startup, not
            // at first wake-up.
            for (list, patterns) in [("show", &plugin.show), ("hide", &plugin.hide)] {
                for pattern in patterns {
                    if let Err(err) = regex_lite::Regex::new(pattern) {
                        return Err(ConfigError::Invalid(format!(
                            "plugins.{name}.{list}: invalid regex {pattern:?}: {err}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Collection period for a plugin (per-plugin override or global).
    pub fn refresh_for(&self, plugin: &str) -> Duration {
        let secs = self
            .plugins
            .get(plugin)
            .and_then(|p| p.refresh)
            .unwrap_or(self.collect.refresh);
        Duration::from_secs_f64(secs)
    }

    /// Idle timeout for a plugin: `idle_cycles` refresh periods (§3).
    pub fn idle_timeout_for(&self, plugin: &str) -> Duration {
        self.refresh_for(plugin) * self.collect.idle_cycles
    }

    pub fn guard_timeout(&self) -> Duration {
        Duration::from_secs_f64(self.collect.guard_timeout)
    }

    pub fn plugin_enabled(&self, plugin: &str) -> bool {
        self.plugins
            .get(plugin)
            .and_then(|p| p.enabled)
            .unwrap_or(true)
    }
}

/// First match wins (order frozen in docs/api.md §7). An explicit path
/// (flag or env var) that does not exist is a startup error, never a
/// silent fallback.
fn discover_path(cli_path: Option<PathBuf>) -> Result<Option<PathBuf>, ConfigError> {
    if let Some(path) = cli_path {
        return if path.is_file() {
            Ok(Some(path))
        } else {
            Err(ConfigError::ExplicitPathMissing(path))
        };
    }
    if let Some(value) = std::env::var_os("GLANCES_RS_CONFIG") {
        let path = PathBuf::from(value);
        return if path.is_file() {
            Ok(Some(path))
        } else {
            Err(ConfigError::ExplicitPathMissing(path))
        };
    }

    let mut candidates = vec![PathBuf::from("glances-rs.toml")];
    if let Some(dir) = xdg_config_dir() {
        candidates.push(dir.join("glances-rs").join("config.toml"));
    }
    if cfg!(unix) {
        candidates.push(PathBuf::from("/etc/glances-rs/config.toml"));
    }
    Ok(candidates.into_iter().find(|p| p.is_file()))
}

fn xdg_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

#[derive(Debug)]
pub enum ConfigError {
    /// A path given via `--config` or `GLANCES_RS_CONFIG` does not exist.
    ExplicitPathMissing(PathBuf),
    Read(PathBuf, std::io::Error),
    Parse(toml::de::Error),
    Invalid(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExplicitPathMissing(path) => {
                write!(f, "config file not found: {}", path.display())
            }
            Self::Read(path, err) => write!(f, "cannot read {}: {err}", path.display()),
            Self::Parse(err) => write!(f, "invalid config: {err}"),
            Self::Invalid(msg) => write!(f, "invalid config: {msg}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Minimal CLI parsing — a dedicated crate would be footprint for three flags.
#[derive(Debug, Default, PartialEq)]
pub struct CliArgs {
    pub config: Option<PathBuf>,
    pub help: bool,
    pub version: bool,
}

pub fn parse_args<I>(args: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut out = CliArgs::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--config" => {
                let path = args.next().ok_or("--config requires a path")?;
                out.config = Some(PathBuf::from(path));
            }
            _ if arg.starts_with("--config=") => {
                out.config = Some(PathBuf::from(&arg["--config=".len()..]));
            }
            "-h" | "--help" => out.help = true,
            "-V" | "--version" => out.version = true,
            other => return Err(format!("unknown argument: {other} (try --help)")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_gives_defaults() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config, Config::default());
        assert!(config.server.bind.is_loopback());
        assert_eq!(config.server.port, 61208);
        assert_eq!(config.server.password, None);
        assert_eq!(config.security.cors_origins, Vec::<String>::new());
        assert_eq!(config.security.trusted_hosts, ["localhost", "127.0.0.1"]);
    }

    #[test]
    fn full_toml_overrides_defaults() {
        let config = Config::from_toml(
            r#"
            [server]
            bind = "0.0.0.0"
            port = 8080
            password = "secret"

            [security]
            cors_origins = ["https://dash.example.com"]
            trusted_hosts = ["monitor.example.com"]

            [collect]
            refresh = 1.5
            idle_cycles = 3
            guard_timeout = 2.0

            [plugins.cpu]
            refresh = 0.5

            [plugins.network]
            enabled = false
            show = ["^eth"]
            hide = ["^lo$"]
            "#,
        )
        .unwrap();
        assert!(!config.server.bind.is_loopback());
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.password.as_deref(), Some("secret"));
        assert_eq!(config.security.trusted_hosts, ["monitor.example.com"]);
        assert_eq!(config.refresh_for("cpu"), Duration::from_millis(500));
        assert_eq!(config.refresh_for("mem"), Duration::from_millis(1500));
        assert_eq!(config.idle_timeout_for("cpu"), Duration::from_millis(1500));
        assert_eq!(config.guard_timeout(), Duration::from_secs(2));
        assert!(!config.plugin_enabled("network"));
        assert!(config.plugin_enabled("cpu"));
        assert_eq!(config.plugins["network"].show, ["^eth"]);
    }

    #[test]
    fn default_idle_timeout_is_five_cycles() {
        let config = Config::default();
        assert_eq!(config.refresh_for("mem"), Duration::from_secs(2));
        assert_eq!(config.idle_timeout_for("mem"), Duration::from_secs(10));
    }

    #[test]
    fn bad_toml_is_an_error() {
        assert!(matches!(
            Config::from_toml("server = ["),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn unknown_field_is_an_error() {
        // deny_unknown_fields: typos must not be silently ignored.
        assert!(matches!(
            Config::from_toml("[server]\nbnd = \"127.0.0.1\""),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn unknown_plugin_section_is_an_error() {
        assert!(matches!(
            Config::from_toml("[plugins.cpus]\nrefresh = 1.0"),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn non_positive_refresh_is_an_error() {
        assert!(matches!(
            Config::from_toml("[collect]\nrefresh = 0.0"),
            Err(ConfigError::Invalid(_))
        ));
        assert!(matches!(
            Config::from_toml("[plugins.cpu]\nrefresh = -1.0"),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn invalid_show_hide_regex_is_an_error() {
        assert!(matches!(
            Config::from_toml("[plugins.network]\nhide = [\"(unclosed\"]"),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn password_env_resolves_the_secret_from_the_environment() {
        // Unique var name so parallel tests don't race on the process env.
        let var = "GLANCES_RS_TEST_PW_OK";
        unsafe { std::env::set_var(var, "from-env") };
        let config = Config::from_toml(&format!("[server]\npassword_env = \"{var}\"")).unwrap();
        assert_eq!(config.server.password.as_deref(), Some("from-env"));
        // The variable name is consumed; only the resolved secret remains.
        assert_eq!(config.server.password_env, None);
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn password_env_missing_variable_is_an_error() {
        let var = "GLANCES_RS_TEST_PW_MISSING";
        unsafe { std::env::remove_var(var) };
        assert!(matches!(
            Config::from_toml(&format!("[server]\npassword_env = \"{var}\"")),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn password_env_empty_variable_is_an_error() {
        let var = "GLANCES_RS_TEST_PW_EMPTY";
        unsafe { std::env::set_var(var, "") };
        assert!(matches!(
            Config::from_toml(&format!("[server]\npassword_env = \"{var}\"")),
            Err(ConfigError::Invalid(_))
        ));
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn password_and_password_env_together_is_an_error() {
        let var = "GLANCES_RS_TEST_PW_BOTH";
        unsafe { std::env::set_var(var, "x") };
        assert!(matches!(
            Config::from_toml(&format!(
                "[server]\npassword = \"clear\"\npassword_env = \"{var}\""
            )),
            Err(ConfigError::Invalid(_))
        ));
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn explicit_missing_path_is_an_error() {
        let missing = PathBuf::from("/nonexistent/glances-rs.toml");
        assert!(matches!(
            Config::load(Some(missing)),
            Err(ConfigError::ExplicitPathMissing(_))
        ));
    }

    #[test]
    fn explicit_path_is_loaded() {
        let path = std::env::temp_dir().join("glances-rs-test-config.toml");
        std::fs::write(&path, "[server]\nport = 9999").unwrap();
        let (config, from) = Config::load(Some(path.clone())).unwrap();
        assert_eq!(config.server.port, 9999);
        assert_eq!(from, Some(path.clone()));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn parse_args_variants() {
        let args = |list: &[&str]| parse_args(list.iter().map(|s| s.to_string()));
        assert_eq!(args(&[]).unwrap(), CliArgs::default());
        assert_eq!(
            args(&["--config", "/tmp/x.toml"]).unwrap().config,
            Some(PathBuf::from("/tmp/x.toml"))
        );
        assert_eq!(
            args(&["--config=/tmp/x.toml"]).unwrap().config,
            Some(PathBuf::from("/tmp/x.toml"))
        );
        assert_eq!(
            args(&["-c", "x.toml"]).unwrap().config,
            Some(PathBuf::from("x.toml"))
        );
        assert!(args(&["--help"]).unwrap().help);
        assert!(args(&["-V"]).unwrap().version);
        assert!(args(&["--config"]).is_err());
        assert!(args(&["--bogus"]).is_err());
    }
}
