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


#[derive(Debug, PartialEq, Clone)]
pub enum CiscoTerminalMode {
    UserExec,
    PrivilegedExec,
    GlobalConfig,
    LineConfig,
    InterfaceConfig(String),
}

pub struct Terminal {
    pub mode: CiscoTerminalMode,
    pub prompt: String,
    pub input: String,
    pub output: String,
    pub end: bool,
}

pub fn switch_mode(mode: &CiscoTerminalMode) -> String {
    match mode {
        CiscoTerminalMode::UserExec => ">".to_string(),
        CiscoTerminalMode::PrivilegedExec => "#".to_string(),
        CiscoTerminalMode::GlobalConfig => "(config)#".to_string(),
        CiscoTerminalMode::LineConfig => "config-line)#".to_string(),
        CiscoTerminalMode::InterfaceConfig(_) => "(config-if)#".to_string(),
    }
}

/// Parses the arguments of a `configure ...` command into the mode it selects.
/// `tokens` is the whole command line split on whitespace.
fn configure_target(tokens: &[&str]) -> Option<CiscoTerminalMode> {
    match tokens.get(1) {
        Some(&"terminal") | Some(&"t") => Some(CiscoTerminalMode::GlobalConfig),
        Some(&"interface") | Some(&"int") => tokens
            .get(2)
            .map(|name| CiscoTerminalMode::InterfaceConfig((*name).to_string())),
        _ => None,
    }
}

pub fn terminal(term: &mut Terminal) {
    let input = term.input.trim().to_string();

    if input.is_empty() {
        term.prompt = switch_mode(&term.mode);
        return;
    }

    let tokens: Vec<&str> = input.split_whitespace().collect();
    // Clone the mode so the match borrows a local, not `term.mode`,
    // which we reassign inside the arms.
    let mode = term.mode.clone();
    let mut known = true;

    match mode {
        CiscoTerminalMode::UserExec => match tokens[0] {
            "disable" => {}
            "enable" | "en" => term.mode = CiscoTerminalMode::PrivilegedExec,
            "exit" => term.end = true,
            "configure" | "conf" => match configure_target(&tokens) {
                Some(m) => term.mode = m,
                None => known = false,
            },
            _ => known = false,
        },

        CiscoTerminalMode::PrivilegedExec => match tokens[0] {
            "disable" => term.mode = CiscoTerminalMode::UserExec,
            "exit" => term.end = true,
            "configure" | "conf" => match configure_target(&tokens) {
                Some(m) => term.mode = m,
                None => known = false,
            },
            _ => known = false,
        },

        CiscoTerminalMode::GlobalConfig => match tokens[0] {
            "exit" | "end" => term.mode = CiscoTerminalMode::PrivilegedExec,
            "interface" | "int" => match tokens.get(1) {
                Some(name) => {
                    term.mode = CiscoTerminalMode::InterfaceConfig((*name).to_string())
                }
                None => known = false,
            },
            "line" => match tokens.get(1) {
                Some(_) => term.mode = CiscoTerminalMode::LineConfig,
                None => known = false,
            },
            _ => known = false,
        },

        CiscoTerminalMode::LineConfig => match tokens[0] {
            "exit" => term.mode = CiscoTerminalMode::GlobalConfig,
            "end" => term.mode = CiscoTerminalMode::PrivilegedExec,
            _ => known = false,
        },

        CiscoTerminalMode::InterfaceConfig(_) => match tokens[0] {
            "exit" => term.mode = CiscoTerminalMode::GlobalConfig,
            "end" => term.mode = CiscoTerminalMode::PrivilegedExec,
            _ => known = false,
        },
    }

    term.output = if known {
        input
    } else {
        format!("\n{input}: command not found")
    };

    term.prompt = switch_mode(&term.mode);
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a Terminal in the given mode, feeds it `input`, and returns it after processing.
    fn run(mode: CiscoTerminalMode, prompt: &str, input: &str) -> Terminal {
        let mut term = Terminal {
            mode,
            prompt: prompt.to_string(),
            input: input.to_string(),
            output: String::new(),
            end: false,
        };
        terminal(&mut term);
        term
    }

    #[test]
    fn test_switch_mode_output() {
        let cases = [
            (CiscoTerminalMode::GlobalConfig, "router(config)#"),
            (CiscoTerminalMode::LineConfig, "router(config-line)#"),
            (
                CiscoTerminalMode::InterfaceConfig("gi0/0/1".to_string()),
                "router(config-if)#",
            ),
            (CiscoTerminalMode::PrivilegedExec, "router#"),
            (CiscoTerminalMode::UserExec, "router>"),
        ];

        for (mode, expected) in cases {
            assert_eq!(switch_mode(&mode).trim(), expected, "mode: {:?}", mode);
        }
    }

    #[test]
    fn test_mode_transitions_from_user_exec() {
        let cases = [
            ("disable", CiscoTerminalMode::UserExec, "router>"),
            ("enable", CiscoTerminalMode::PrivilegedExec, "router#"),
            ("en", CiscoTerminalMode::PrivilegedExec, "router#"),
            ("configure terminal", CiscoTerminalMode::GlobalConfig, "router(config)#"),
            ("conf t", CiscoTerminalMode::GlobalConfig, "router(config)#"),
            (
                "configure interface gi0/0/0",
                CiscoTerminalMode::InterfaceConfig("gi0/0/0".to_string()),
                "router(config-if)#",
            ),
            (
                "configure interface gi0/0/1",
                CiscoTerminalMode::InterfaceConfig("gi0/0/1".to_string()),
                "router(config-if)#",
            ),
        ];

        for (input, expected_mode, expected_prompt) in cases {
            let term = run(CiscoTerminalMode::UserExec, "router>", input);
            assert_eq!(term.mode, expected_mode, "input: {input:?}");
            assert_eq!(term.prompt.trim(), expected_prompt, "input: {input:?}");
            assert_eq!(term.output, input, "input: {input:?}");
        }
    }

    #[test]
    fn test_line_config_transition() {
        let term = run(CiscoTerminalMode::GlobalConfig, "router(config)#", "line vty 0 4");
        assert_eq!(term.mode, CiscoTerminalMode::LineConfig);
        assert_eq!(term.prompt.trim(), "router(config-line)#");
        assert_eq!(term.output, "line vty 0 4");

        let term = run(CiscoTerminalMode::LineConfig, "router(config-line)#", "exit");
        assert_eq!(term.mode, CiscoTerminalMode::GlobalConfig);
        assert_eq!(term.prompt.trim(), "router(config)#");
    }

    #[test]
    fn test_unknown_command_in_user_exec() {
        let term = run(CiscoTerminalMode::UserExec, "router>", "UnknownMode");
        assert_eq!(term.mode, CiscoTerminalMode::UserExec);
        assert_eq!(term.prompt.trim(), "router>");
        assert_eq!(term.output, "\nUnknownMode: command not found");
    }

    #[test]
    fn test_unknown_command_keeps_privileged_exec() {
        let term = run(CiscoTerminalMode::PrivilegedExec, "router#", "UnknownMode");
        assert_eq!(term.mode, CiscoTerminalMode::PrivilegedExec);
        assert_eq!(term.prompt.trim(), "router#");
        assert_eq!(term.output, "\nUnknownMode: command not found");
    }

    #[test]
    fn test_empty_input() {
        let term = run(CiscoTerminalMode::UserExec, "router>", "");
        assert_eq!(term.mode, CiscoTerminalMode::UserExec);
        assert_eq!(term.prompt.trim(), "router>");
        assert_eq!(term.output, "");
    }

    #[test]
    fn test_terminal_exit() {
        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "exit".to_string(),
            output: String::new(),
            end: false,
        };
        assert!(!term.end);
        terminal(&mut term);
        assert!(term.end);
    }
}