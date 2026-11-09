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

/// Network device vendor: selects the command tree (and, for the local
/// simulator, the prompt style).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Cisco,
    Juniper,
    Mikrotik,
    Other,
}

impl Vendor {
    pub fn all() -> &'static [Vendor] {
        &[
            Vendor::Cisco,
            Vendor::Juniper,
            Vendor::Mikrotik,
            Vendor::Other,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Vendor::Cisco => "Cisco",
            Vendor::Juniper => "Juniper",
            Vendor::Mikrotik => "Mikrotik",
            Vendor::Other => "Unsupported",
        }
    }

    pub fn parse(s: &str) -> Vendor {
        match s.to_ascii_lowercase().as_str() {
            "cisco" | "ios" => Vendor::Cisco,
            "juniper" | "junos" => Vendor::Juniper,
            "mikrotik" | "routeros" => Vendor::Mikrotik,
            _ => Vendor::Other,
        }
    }
}

/// Command templates per terminal mode, loaded from `commands-<vendor>.yaml`.
pub struct CommandTree {
    modes: BTreeMap<String, Vec<String>>,
}

impl CommandTree {
    /// The command tree for `vendor` is baked into the binary.
    pub fn load(vendor: Vendor) -> Self {
        let text = match vendor {
            Vendor::Cisco => include_str!("../commands/cisco.yaml"),
            Vendor::Juniper => include_str!("../commands/juniper.yaml"),
            Vendor::Mikrotik => include_str!("../commands/mikrotik.yaml"),
            // Unknown vendor: no command templates, autocomplete finds nothing.
            Vendor::Other => return Self { modes: BTreeMap::new() },
        };
        let modes: BTreeMap<String, Vec<String>> =
            serde_yml::from_str(text).expect("invalid commands yaml");
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

    /// All commands available in `mode` that match `input`.
    ///
    /// Matching is token-based, like IOS abbreviations: every whitespace
    /// token of `input` must be a prefix of the corresponding token of the
    /// command, so `conf t` matches `configure terminal` and `in` matches
    /// `interface Gi0/0/`.
    pub fn autocomplete(&self, mode: &CiscoTerminalMode, input: &str) -> Vec<String> {
        match self.modes.get(Self::key(mode)) {
            Some(commands) => commands
                .iter()
                .filter(|c| command_matches(input, c))
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }
}

/// True when `input` is an IOS-style abbreviation of `command`: every input
/// token is a prefix of the corresponding command token.
pub fn command_matches(input: &str, command: &str) -> bool {
    let input_tokens: Vec<&str> = input.split_whitespace().collect();
    let command_tokens: Vec<&str> = command.split_whitespace().collect();
    input_tokens.len() <= command_tokens.len()
        && input_tokens
            .iter()
            .zip(command_tokens.iter())
            .all(|(i, c)| c.starts_with(i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vendor_parse_and_labels() {
        assert_eq!(Vendor::parse("junos"), Vendor::Juniper);
        assert_eq!(Vendor::parse("RouterOS"), Vendor::Mikrotik);
        assert_eq!(Vendor::parse("cisco"), Vendor::Cisco);
        assert_eq!(Vendor::parse("ios"), Vendor::Cisco);
        assert_eq!(Vendor::parse("FortiNet"), Vendor::Other);
        assert_eq!(Vendor::parse("Aruba"), Vendor::Other);
        assert_eq!(Vendor::parse("HP"), Vendor::Other);
        assert_eq!(Vendor::parse("CheckPoint"), Vendor::Other);
        assert_eq!(Vendor::parse("--; DROP ALL;"), Vendor::Other);
        assert_eq!(Vendor::parse("rm -rf"), Vendor::Other);
        assert_eq!(Vendor::all().len(), 4);
        assert_eq!(Vendor::Cisco.label(), "Cisco");
        assert_eq!(Vendor::Other.label(), "Unsupported");
    }

    #[test]
    fn test_other_vendor_loads_empty_tree_instead_of_panicking() {
        let tree = CommandTree::load(Vendor::Other);
        assert!(tree.autocomplete(&CiscoTerminalMode::UserExec, "show").is_empty());
    }

    #[test]
    fn test_autocomplete_filters_by_mode_and_prefix() {
        let tree = CommandTree::load(Vendor::Cisco);

        let user = tree.autocomplete(&CiscoTerminalMode::UserExec, "en");
        assert_eq!(user, vec!["enable".to_string()]);

        let priv_cmds = tree.autocomplete(&CiscoTerminalMode::PrivilegedExec, "conf");
        assert!(priv_cmds.contains(&"configure terminal".to_string()));
        assert!(priv_cmds.contains(&"configure interface".to_string()));

        assert!(tree
            .autocomplete(&CiscoTerminalMode::UserExec, "zzzz")
            .is_empty());
    }

    #[test]
    fn test_autocomplete_matches_token_abbreviations() {
        let tree = CommandTree::load(Vendor::Cisco);

        assert_eq!(
            tree.autocomplete(&CiscoTerminalMode::PrivilegedExec, "conf t"),
            vec!["configure terminal".to_string()]
        );

        assert_eq!(
            tree.autocomplete(&CiscoTerminalMode::GlobalConfig, "in"),
            vec!["interface Gi0/0/".to_string()]
        );

        assert_eq!(
            tree.autocomplete(&CiscoTerminalMode::UserExec, "sh ip int b"),
            vec!["show ip interface brief".to_string()]
        );

        // A token that is not a prefix of the corresponding command token
        // must not match.
        assert!(tree
            .autocomplete(&CiscoTerminalMode::PrivilegedExec, "conf x")
            .is_empty());
    }

    #[test]
    fn test_all_vendor_trees_load() {
        for v in Vendor::all() {
            if *v == Vendor::Other {
                continue; // unknown vendor: empty tree by design
            }
            let tree = CommandTree::load(*v);
            assert!(!tree
                .autocomplete(&CiscoTerminalMode::UserExec, "")
                .is_empty());
        }
    }
}

