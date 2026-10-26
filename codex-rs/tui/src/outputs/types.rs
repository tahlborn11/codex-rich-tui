//! Data shared by the read-only Outputs browser and its loaders.
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Category {
    #[default]
    Changes,
    Agents,
    Files,
}
impl Category {
    pub(crate) const ALL: [Self; 3] = [Self::Changes, Self::Agents, Self::Files];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Changes => "Changes",
            Self::Agents => "Agents",
            Self::Files => "Files",
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Agent {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) summary: String,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct ConversationData {
    pub(crate) history_loaded: bool,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) agents: Vec<Agent>,
    pub(crate) notice: Option<String>,
    pub(crate) turn_patch: Option<String>,
    pub(crate) branch_creations: Vec<crate::branch_association::CreatedBranch>,
    /// Checkout/file locations actually used by this chat, independent of shell syntax.
    pub(crate) branch_uses: Vec<crate::branch_association::BranchUse>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Repository {
    pub(crate) root: PathBuf,
    pub(crate) identity: String,
    pub(crate) branch: Option<String>,
    pub(crate) head: Option<String>,
    pub(crate) branches: Vec<String>,
    pub(crate) default_base: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ChangeKind {
    #[default]
    Branch,
    Working,
    Staged,
    TurnPatch,
}
impl ChangeKind {
    pub(crate) const ALL: [Self; 4] = [Self::Branch, Self::Working, Self::Staged, Self::TurnPatch];
}
