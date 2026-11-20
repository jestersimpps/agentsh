use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Read,
    Write,
    Delete,
    Exec,
    Net,
}

impl Capability {
    pub const ALL: [Capability; 5] =
        [Capability::Read, Capability::Write, Capability::Delete, Capability::Exec, Capability::Net];

    pub fn as_str(&self) -> &'static str {
        match self {
            Capability::Read => "read",
            Capability::Write => "write",
            Capability::Delete => "delete",
            Capability::Exec => "exec",
            Capability::Net => "net",
        }
    }

    pub fn parse_list(list: &str) -> Result<Vec<Capability>, String> {
        list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::parse).collect()
    }
}

impl FromStr for Capability {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Capability::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| format!("unknown capability `{s}` (valid: read, write, delete, exec, net)"))
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
