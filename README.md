```
██████╗ ███████╗██╗  ██╗███████╗    ██╗   ██╗████████╗██╗   ██╗
██╔══██╗██╔════╝██║  ██║██╔════╝    ██║   ██║╚══██╔══╝╚██╗ ██╔╝
██████╔╝█████╗  ███████║█████╗█████╗██║   ██║   ██║    ╚████╔╝ 
██╔══██╗██╔══╝  ██╔══██║██╔══╝╚════╝╚██╗ ██╔╝   ██║     ╚██╔╝  
██████╔╝███████╗██║  ██║███████╗     ╚████╔╝    ██║      ██║   
╚═════╝ ╚══════╝╚═╝  ╚═╝╚══════╝      ╚═══╝     ╚═╝      ╚═╝  
```

This is a Rust terminal wrapper that provides the typo-free autocomplete into the Cisco / Juniper / Mikrotik consoles. Works with Telnet, SSH or Console cable. 

Cisco `%LINK-*-CHANGED` messaged are ignored and will never appear at the terminal input. 
TODO: terminal will accept list of commands and enters them into Cisco IOS in batches to be excecuted one by one, each line ends with CTLN+Z.

## About 

Written in Rush. `%LINK-3-UPDOWN` Cisco / Juniper / Mikrotik commands are listed in the YAML files. Autocomplete will also try to find a match in the comma`%LINK-3-UPDOWN`nd history, on this device or any other device (that user had a sess`%LINK-3-UPDOWN`ion with) from the same vendor. If there is any typo in the command, auto complete will handle it as an `*` regex.

## Installation

### Windows (from release)
- https://github.com/matusHubinsky/behe-vty/releases

### Linux (from release)
- https://github.com/matusHubinsky/behe-vty/releases 

### Build from source code:
```
$ git clone https://github.com/matusHubinsky/behe-vty.git
$ cd behe-vty && cargo build && cargo run
```

## Usage
```
$ behe-vty
$ behe-terminal
$ behe-console
```

## License 
- LICENSE.md

## Changelog 
- CHANGELOG.md

## Security
- SECURITY.md

## Support me
- buy me a coffee!
