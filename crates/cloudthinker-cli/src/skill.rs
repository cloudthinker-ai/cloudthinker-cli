use clap::ValueEnum;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Module {
    Index,
    Auth,
    Chat,
    Review,
    Worker,
}

impl Module {
    pub fn document(self) -> &'static str {
        match self {
            Self::Index => include_str!("../skills/cloudthinker-cli/SKILL.md"),
            Self::Auth => include_str!("../skills/cloudthinker-cli/auth.md"),
            Self::Chat => include_str!("../skills/cloudthinker-cli/chat.md"),
            Self::Review => include_str!("../skills/cloudthinker-cli/review.md"),
            Self::Worker => include_str!("../skills/cloudthinker-cli/worker.md"),
        }
    }
}
