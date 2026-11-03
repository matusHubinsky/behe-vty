#[derive(Debug, PartialEq, Clone)]
pub enum CiscoTerminalMode {
    UserExec,
    PrivilegedExec,
    GlobalConfig,
    LineConfig,
    InterfaceConfig(String), // Associated data for interface ID
}

#[derive(Debug, PartialEq)]
pub struct Terminal {
    pub mode: CiscoTerminalMode,
    pub prompt: String,
    pub input: String,
    pub output: String,
    pub end: bool,
}

pub fn switch_mode(mode: &CiscoTerminalMode) -> String {
    match mode {
        CiscoTerminalMode::UserExec => "router>".to_string(),
        CiscoTerminalMode::PrivilegedExec => "router#".to_string(),
        CiscoTerminalMode::GlobalConfig => "router(config)#".to_string(),
        CiscoTerminalMode::LineConfig => "router(config-line)#".to_string(),
        CiscoTerminalMode::InterfaceConfig(_interface) => "router(config-if)#".to_string(),
    }
}

// ----------------------------------------------------------------------
// todo I need to determine what kind of code I am looking at.
// terminal_change_mode
// terminal_show
// terminal_configure
// terminal_security
// terminal_administration
// ----------------------------------------------------------------------
// rename thins to parser
// or some other fance way
pub fn terminal(term: &mut Terminal) -> &mut Terminal {
    term.output = term.input.clone();
    term.end = false;

    let mut valid = true;

    match term.input.as_str() {
        "show ip interface brief" => {
            term.output = String::from(
                "\nInterface\tIP-Address\tOK?\tMethod\tStatus\tProtocol\n\
                Gig0/0\t192.168.1.1\tYES\tmanual\tup\tup\n\
                Gig0/1\tunassigned\tNO\tunset\tdown\tdown",
            );
        }
        "show version" => {
            term.output = String::from(
                "\nCisco IOS Software, C800 Software (C800-UNIVERSALK9-M), Version 15.5(3)M\n\
                Technical Support: http://www.cisco.com/techsupport\n\
                ROM: System Bootstrap, Version 12.4(22r)YB\n\
                System image file is 'flash:c800-universalk9-mz.SPA.155-3.M.bin'\n\
                Uptime is 3 weeks, 2 days, 5 hours, 22 minutes",
            );
        }
        "show running-config" => {
            term.output = String::from(
                "\nBuilding configuration...\n\
                Current configuration : 1024 bytes\n\
                version 15.5\n\
                hostname Router\n\
                interface Gig0/0\n\
                \tip address 192.168.1.1 255.255.255.0\n\
                \tno shutdown\n\
                interface Gig0/1\n\
                \tshutdown\n\
                line vty 0 4\n\
                \tpassword cisco\n\
                \tlogin",
            );
        }
        "show interfaces" => {
            term.output = String::from(
                "\nGig0/0 is up, line protocol is up\n\
                \tHardware is Gigabit Ethernet\n\
                \tInternet address is 192.168.1.1/24\n\
                \tMTU 1500 bytes, BW 1000000 Kbit/sec\n\
                Gig0/1 is administratively down, line protocol is down\n\
                \tHardware is Gigabit Ethernet\n\
                \tMTU 1500 bytes, BW 1000000 Kbit/sec",
            );
        }
        "show ip route" => {
            term.output = String::from(
                "\nCodes: C - connected, S - static, R - RIP, M - mobile\n\
                C  192.168.1.0/24 is directly connected, Gig0/0\n\
                S  0.0.0.0/0 [1/0] via 192.168.1.254",
            );
        }
        _ => {
            valid = false;
        }
    }

    let new_mode = match term.input.as_str() {
        "enable" | "en" => {
            if term.mode == CiscoTerminalMode::UserExec {
                CiscoTerminalMode::PrivilegedExec
            } else {
                term.output = "\nYou are not in the User mode.".to_string();
                term.mode.clone()
            }
        }
        "disable" | "dis" => CiscoTerminalMode::UserExec,
        "line vty 0 4" => {
            if term.mode == CiscoTerminalMode::PrivilegedExec {
                CiscoTerminalMode::LineConfig
            } else {
                term.output = "\nYou are not in the Priviledged mode.".to_string();
                term.mode.clone()
            }
        }
        "configure terminal" | "conf t" => {
            if term.mode == CiscoTerminalMode::PrivilegedExec {
                CiscoTerminalMode::GlobalConfig
            } else {
                term.output = "\nYou are not in the Priviledged mode.".to_string();
                term.mode.clone()
            }
        }          
        s if s.starts_with("configure interface ") => {
            let interface = &s["configure interface ".len()..];
            CiscoTerminalMode::InterfaceConfig(interface.to_string())
        }
        "exit" => {
            println!("EXIT was pressed\n");
            term.end = true;
            term.mode.clone()
        }
        "" => {
            term.output = term.input.clone();
            term.mode.clone()
        }
        _ => {
            if !valid {
                term.output = format!("\n{}: command not found", term.input.clone());
            }
            term.mode.clone()
        }
    };

    term.mode = new_mode.clone();
    term.prompt = switch_mode(&new_mode);
    term
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_switch_mode_output() {
        let mode1 = CiscoTerminalMode::GlobalConfig;
        let output1 = switch_mode(&mode1);
        assert_eq!(output1.trim(), "router(config)#");

        let mode2 = CiscoTerminalMode::InterfaceConfig("gi0/0/1".to_string());
        let output2 = switch_mode(&mode2);
        assert_eq!(output2.trim(), "router(config-if)#");

        let mode3 = CiscoTerminalMode::PrivilegedExec;
        let output3 = switch_mode(&mode3);
        assert_eq!(output3.trim(), "router#");

        let mode4 = CiscoTerminalMode::UserExec;
        let output4 = switch_mode(&mode4);
        assert_eq!(output4.trim(), "router>");
    }

    #[test]
    fn test_terminal_function() {
        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "disable".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::UserExec);
        assert_eq!(term.prompt.trim(), "router>");
        assert_eq!(term.output, "disable");

        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "enable".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::PrivilegedExec);
        assert_eq!(term.prompt.trim(), "router#");
        assert_eq!(term.output, "enable");

        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "configure terminal".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::GlobalConfig);
        assert_eq!(term.prompt.trim(), "router(config)#");
        assert_eq!(term.output, "configure terminal");

        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "configure interface gi0/0/0".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::InterfaceConfig("gi0/0/0".to_string()));
        assert_eq!(term.prompt.trim(), "router(config-if)#");
        assert_eq!(term.output, "configure interface gi0/0/0");

        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "en".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::PrivilegedExec);
        assert_eq!(term.prompt.trim(), "router#");
        assert_eq!(term.output, "en");

        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "conf t".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::GlobalConfig);
        assert_eq!(term.prompt.trim(), "router(config)#");
        assert_eq!(term.output, "conf t");
    }

    #[test]
    fn test_unknown_terminal_mode() {
        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "UnknownMode".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::UserExec);
        assert_eq!(term.prompt.trim(), "router>");
        assert_eq!(term.output, "\nUnknownMode: command not found");
    }

    #[test]
    fn test_mode_stays_same() {
        let mut term = Terminal {
            mode: CiscoTerminalMode::PrivilegedExec,
            prompt: "router#".to_string(),
            input: "UnknownMode".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::PrivilegedExec);
        assert_eq!(term.prompt.trim(), "router#");
        assert_eq!(term.output, "\nUnknownMode: command not found");
    }

    #[test]
    fn test_empty_input() {
        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::UserExec);
        assert_eq!(term.prompt.trim(), "router>");
        assert_eq!(term.output, "");
    }

    #[test]
    fn test_interface_config_mode() {
        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "configure interface gi0/0/1".to_string(),
            output: "".to_string(),
            end: false,
        };
        terminal(&mut term);
        assert_eq!(term.mode, CiscoTerminalMode::InterfaceConfig("gi0/0/1".to_string()));
        assert_eq!(term.prompt.trim(), "router(config-if)#");
        assert_eq!(term.output, "configure interface gi0/0/1");
    }

    #[test]
    fn test_terminal_exit() {
        let mut term = Terminal {
            mode: CiscoTerminalMode::UserExec,
            prompt: "router>".to_string(),
            input: "exit".to_string(),
            output: "".to_string(),
            end: false,
        };
        assert_eq!(term.end, false);
        terminal(&mut term);
        assert_eq!(term.end, true);
    }
}