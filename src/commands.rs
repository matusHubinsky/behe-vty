// Copyright (c) 2026 - Matus Hubinsky

// Licensed under the EUPL-1.2-only
// You may not use this work except in compliance with the Licence.
// You may obtain a copy of the Licence at:

// https://joinup.ec.europa.eu/software/page/eupl

// Unless required by applicable law or agreed to in writing, software
// distributed under the Licence is distributed on an "AS IS" basis,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the Licence for the specific language governing permissions and
// limitations under the Licence.

use crate::cisco::CiscoTerminalMode;
use std::collections::BTreeMap;

/// Command templates per terminal mode, loaded from `commands-cisco.yaml`.
pub struct CommandTree {
    modes: BTreeMap<String, Vec<String>>,
}

impl CommandTree {
    /// The default tree is baked into the binary.
    pub fn load() -> Self {
        let text = include_str!("../commands/cisco.yaml");
        let modes: BTreeMap<String, Vec<String>> =
            serde_yml::from_str(text).expect("invalid commands-cisco.yaml");
        Self { modes }
    }

    fn key(mode: &CiscoTerminalMode) -> &'static str {
        match mode {
            CiscoTerminalMode::UserExec => "user_exec",
            CiscoTerminalMode::PrivilegedExec => "privileged_exec",
            CiscoTerminalMode::GlobalConfig => "global_config",
            CiscoTerminalMode::LineConfig => "line_config",
            CiscoTerminalMode::InterfaceConfig(_) => "interface_config",
        }
    }

    /// All commands available in `mode` that start with `prefix`.
    pub fn autocomplete(&self, mode: &CiscoTerminalMode, prefix: &str) -> Vec<String> {
        match self.modes.get(Self::key(mode)) {
            Some(commands) => commands
                .iter()
                .filter(|c| c.starts_with(prefix))
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }
}