pub const LOGIN_MODE_ENV_VAR: &str = "CLOUDTHINKER_LOGIN";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginEnvironment {
    pub device_auth_flag: bool,
    pub mode_override: Option<String>,
    pub ssh: bool,
    pub browser_helper: bool,
    pub display: bool,
    pub wsl: bool,
    pub platform: Platform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFlow {
    Browser,
    Device,
    HeadlessDevice(&'static str),
}

impl LoginEnvironment {
    pub fn from_process(device_auth_flag: bool) -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        Self {
            device_auth_flag,
            mode_override: std::env::var(LOGIN_MODE_ENV_VAR).ok(),
            ssh: set("SSH_CONNECTION") || set("SSH_CLIENT") || set("SSH_TTY"),
            browser_helper: set("BROWSER"),
            display: set("DISPLAY") || set("WAYLAND_DISPLAY"),
            wsl: set("WSL_DISTRO_NAME") || set("WSL_INTEROP"),
            platform: if cfg!(target_os = "linux") {
                Platform::Linux
            } else {
                Platform::Other
            },
        }
    }
}

pub fn choose(environment: &LoginEnvironment) -> LoginFlow {
    if environment.device_auth_flag {
        return LoginFlow::Device;
    }
    match environment
        .mode_override
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("browser") => return LoginFlow::Browser,
        Some("device") => return LoginFlow::Device,
        _ => {}
    }
    if environment.browser_helper || environment.wsl {
        return LoginFlow::Browser;
    }
    if environment.ssh {
        return LoginFlow::HeadlessDevice("SSH session");
    }
    if environment.platform == Platform::Linux && !environment.display {
        return LoginFlow::HeadlessDevice("no display");
    }
    LoginFlow::Browser
}

pub fn switch_line(reason: &str) -> String {
    format!(
        "No local browser detected ({reason}). Using device-code login. Set {LOGIN_MODE_ENV_VAR}=browser to use the browser callback instead."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop(platform: Platform) -> LoginEnvironment {
        LoginEnvironment {
            device_auth_flag: false,
            mode_override: None,
            ssh: false,
            browser_helper: false,
            display: true,
            wsl: false,
            platform,
        }
    }

    #[test]
    fn each_environment_picks_its_login_flow() {
        let linux = desktop(Platform::Linux);
        let cases = [
            (
                LoginEnvironment {
                    ssh: true,
                    display: false,
                    ..linux.clone()
                },
                LoginFlow::HeadlessDevice("SSH session"),
            ),
            (
                LoginEnvironment {
                    ssh: true,
                    platform: Platform::Other,
                    ..linux.clone()
                },
                LoginFlow::HeadlessDevice("SSH session"),
            ),
            (
                LoginEnvironment {
                    display: false,
                    ..linux.clone()
                },
                LoginFlow::HeadlessDevice("no display"),
            ),
            (
                LoginEnvironment {
                    display: false,
                    wsl: true,
                    ..linux.clone()
                },
                LoginFlow::Browser,
            ),
            (
                LoginEnvironment {
                    ssh: true,
                    display: false,
                    browser_helper: true,
                    ..linux.clone()
                },
                LoginFlow::Browser,
            ),
            (
                LoginEnvironment {
                    display: false,
                    ..desktop(Platform::Other)
                },
                LoginFlow::Browser,
            ),
            (linux.clone(), LoginFlow::Browser),
            (
                LoginEnvironment {
                    device_auth_flag: true,
                    mode_override: Some("browser".into()),
                    ..linux.clone()
                },
                LoginFlow::Device,
            ),
            (
                LoginEnvironment {
                    ssh: true,
                    display: false,
                    mode_override: Some("Browser".into()),
                    ..linux.clone()
                },
                LoginFlow::Browser,
            ),
            (
                LoginEnvironment {
                    mode_override: Some("device".into()),
                    ..linux.clone()
                },
                LoginFlow::Device,
            ),
            (
                LoginEnvironment {
                    ssh: true,
                    mode_override: Some("unknown".into()),
                    ..linux
                },
                LoginFlow::HeadlessDevice("SSH session"),
            ),
        ];
        for (environment, expected) in cases {
            assert_eq!(choose(&environment), expected, "{environment:?}");
        }
    }
}
