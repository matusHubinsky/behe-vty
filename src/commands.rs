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

use serde::Deserialize;
use std::collections::HashMap;

use crate::cisco::CiscoTerminalMode;

#[derive(Deserialize)]
pub struct CommandTree(HashMap<String, Vec<String>>);

impl CommandTree {
    /// The default tree is baked into the binary.
    pub fn load() -> Self {
        let text = include_str!("../commands-cisco.yaml");
        serde_yml::from_str(text).expect("invalid commands.yaml")
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

    /// All commands that start with `input` in the given mode, sorted.
    pub fn autocomplete(&self, mode: &CiscoTerminalMode, input: &str) -> Vec<String> {
        let mut matches: Vec<String> = self
            .0
            .get(Self::key(mode))
            .map(|cmds| {
                cmds.iter()
                    .filter(|c| c.starts_with(input))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        matches.sort();
        matches
    }
}
