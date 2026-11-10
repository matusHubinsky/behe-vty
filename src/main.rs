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

mod cisco;
mod commands;
mod database;
mod tui;
mod vault;
mod networking;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const NAME: &str = env!("CARGO_PKG_NAME");
pub const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");
pub const AUTHORS: &str = env!("CARGO_PKG_AUTHORS");
pub const LICENSE: &str = env!("CARGO_PKG_LICENSE");
pub const HOMEPAGE: &str = env!("CARGO_PKG_HOMEPAGE");

fn print_version() {
    println!("{NAME} {VERSION}");
    println!("{DESCRIPTION}");
    println!("{AUTHORS} | {LICENSE} | {HOMEPAGE}");
}

fn main() {
    print_version();
    #[cfg(windows)]
    {
        // VERSIONINFO wants four numeric parts: 1.0.4 -> 1.0.4.0
        let ver = env!("CARGO_PKG_VERSION");
        let file_version = if ver.matches('.').count() == 2 {
            format!("{ver}.0")
        } else {
            ver.to_string()
        };
        winres::WindowsResource::new()
            .set("FileVersion", &file_version)
            .set("ProductVersion", &file_version)
            .compile()
            .expect("failed to embed Windows version resource");
    }
    println!("cargo:rerun-if-changed=build.rs");
    tui::run();
}