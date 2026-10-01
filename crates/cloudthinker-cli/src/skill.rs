use clap::ValueEnum;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Module {
    Index,
    Auth,
    Chat,
    Cloud,
    Review,
    Cyber,
    CyberScan,
    CyberVerify,
    CyberReport,
    Worker,
}

impl Module {
    pub fn document(self) -> &'static str {
        match self {
            Self::Index => include_str!("../skills/cloudthinker-cli/SKILL.md"),
            Self::Auth => include_str!("../skills/cloudthinker-cli/auth.md"),
            Self::Cloud => include_str!("../skills/cloudthinker-cli/cloud.md"),
            Self::Chat => include_str!("../skills/cloudthinker-cli/chat.md"),
            Self::Review => include_str!("../skills/cloudthinker-cli/review.md"),
            Self::Cyber => include_str!("../skills/cloudthinker-cli/cyber.md"),
            Self::CyberScan => include_str!("../skills/cloudthinker-cli/cyber-scan.md"),
            Self::CyberVerify => include_str!("../skills/cloudthinker-cli/cyber-verify.md"),
            Self::CyberReport => include_str!("../skills/cloudthinker-cli/cyber-report.md"),
            Self::Worker => include_str!("../skills/cloudthinker-cli/worker.md"),
        }
    }
}
