use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AccessValue", into = "AccessValue")]
pub enum AgentAccess {
    #[default]
    Default,
    DenyAll,
    DenyEscalation {
        acp_mode: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AccessValue {
    Name(AccessName),
    Restricted(RestrictedAccess),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AccessName {
    Default,
    DenyAll,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestrictedAccess {
    mode: RestrictedMode,
    acp_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RestrictedMode {
    DenyEscalation,
}

impl TryFrom<AccessValue> for AgentAccess {
    type Error = &'static str;

    fn try_from(value: AccessValue) -> Result<Self, Self::Error> {
        match value {
            AccessValue::Name(AccessName::Default) => Ok(Self::Default),
            AccessValue::Name(AccessName::DenyAll) => Ok(Self::DenyAll),
            AccessValue::Restricted(RestrictedAccess { acp_mode, .. })
                if !acp_mode.trim().is_empty() =>
            {
                Ok(Self::DenyEscalation { acp_mode })
            }
            _ => Err("deny_escalation acp_mode must not be blank"),
        }
    }
}

impl From<AgentAccess> for AccessValue {
    fn from(access: AgentAccess) -> Self {
        match access {
            AgentAccess::Default => Self::Name(AccessName::Default),
            AgentAccess::DenyAll => Self::Name(AccessName::DenyAll),
            AgentAccess::DenyEscalation { acp_mode } => Self::Restricted(RestrictedAccess {
                mode: RestrictedMode::DenyEscalation,
                acp_mode,
            }),
        }
    }
}

impl AgentAccess {
    pub fn allows_tools(&self) -> bool {
        !matches!(self, Self::DenyAll)
    }

    pub fn denies_permissions(&self) -> bool {
        !matches!(self, Self::Default)
    }

    pub fn required_mode(&self) -> Option<&str> {
        match self {
            Self::DenyEscalation { acp_mode } => Some(acp_mode),
            _ => None,
        }
    }
}
