//! Data shared by the read-only Outputs browser and its loaders.
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Category {
    #[default]
    Plans,
    Files,
    Changes,
    Agents,
}
impl Category {
    pub(crate) const ALL: [Self; 4] = [Self::Plans, Self::Files, Self::Changes, Self::Agents];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Plans => "Plans",
            Self::Files => "Files",
            Self::Changes => "Changes",
            Self::Agents => "Agents",
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Document {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) markdown: String,
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
    pub(crate) plans: Vec<Document>,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) agents: Vec<Agent>,
    pub(crate) notice: Option<String>,
    pub(crate) turn_patch: Option<String>,
    pub(crate) branch_creations: Vec<crate::branch_association::CreatedBranch>,
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
