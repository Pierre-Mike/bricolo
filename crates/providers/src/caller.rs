use crate::disk::DiskGrant;
use crate::shell::ShellGrant;

/// Who is calling: the plugin's name and the grants that scope its calls.
#[derive(Clone, Debug, Default)]
pub struct Caller {
    pub plugin: String,
    pub disk: Vec<DiskGrant>,
    /// `[shell]` in `plugin.toml`: the programs and variables it may use.
    pub shell: ShellGrant,
    /// `[http] allow` in `plugin.toml`: the `host:port` pairs it may reach.
    pub http: Vec<String>,
}

impl Caller {
    pub fn new(plugin: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            ..Self::default()
        }
    }
}
