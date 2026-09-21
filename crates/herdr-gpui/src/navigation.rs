//! Where a focus request is aimed. A closed set of targets, so navigation is
//! never a boolean-plus-ID pair or a string tag.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NavigationTarget<T> {
    Workspace(T),
    Tab(T),
    Pane(T),
}

pub(crate) type OwnedNavigationTarget = NavigationTarget<String>;

impl<T: AsRef<str>> NavigationTarget<T> {
    pub(crate) fn as_ref(&self) -> NavigationTarget<&str> {
        match self {
            Self::Workspace(id) => NavigationTarget::Workspace(id.as_ref()),
            Self::Tab(id) => NavigationTarget::Tab(id.as_ref()),
            Self::Pane(id) => NavigationTarget::Pane(id.as_ref()),
        }
    }

    pub(crate) fn to_owned(&self) -> OwnedNavigationTarget {
        match self.as_ref() {
            NavigationTarget::Workspace(id) => NavigationTarget::Workspace(id.to_owned()),
            NavigationTarget::Tab(id) => NavigationTarget::Tab(id.to_owned()),
            NavigationTarget::Pane(id) => NavigationTarget::Pane(id.to_owned()),
        }
    }
}
