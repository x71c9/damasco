//! Permissions as capabilities: what a role may do on a resource, in an
//! environment. The three roles are fixed sets of capabilities; rules that
//! depend on the target (nobody acts on the owner but the owner) live with
//! the operations in `people`.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Admin,
    Editor,
}

impl Role {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Role::Owner),
            "admin" => Some(Role::Admin),
            "editor" => Some(Role::Editor),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Editor => "editor",
        }
    }
}

/// Where the action happens: accounts belong to the whole installation,
/// content and code to preview or production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Installation,
    Preview,
    #[allow(dead_code)]
    Production,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    People,
    Activity,
    Chat,
    Content,
    Publish,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Write,
}

/// Whether `role` may perform `action` on `resource` in `scope`.
pub fn allows(role: Role, scope: Scope, resource: Resource, action: Action) -> bool {
    use Resource::*;
    match role {
        // Owner and administrators have the same capabilities; what tells
        // them apart is that only the owner acts on the owner's account.
        Role::Owner | Role::Admin => true,
        Role::Editor => match resource {
            Content => scope != Scope::Installation,
            Publish => action == Action::Write && scope != Scope::Installation,
            People | Activity | Chat | Settings => false,
        },
    }
}

/// One capability, as the panel receives it to show or hide sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capability {
    pub resource: Resource,
    pub action: Action,
}

/// Capabilities of a role in the preview and installation scopes, the ones
/// the panel uses today.
pub fn capabilities(role: Role) -> Vec<Capability> {
    let resources = [
        (Resource::People, Scope::Installation),
        (Resource::Activity, Scope::Installation),
        (Resource::Settings, Scope::Installation),
        (Resource::Chat, Scope::Preview),
        (Resource::Content, Scope::Preview),
        (Resource::Publish, Scope::Preview),
    ];
    let mut list = Vec::new();
    for (resource, scope) in resources {
        for action in [Action::Read, Action::Write] {
            if allows(role, scope, resource, action) {
                list.push(Capability { resource, action });
            }
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_and_admin_can_do_everything() {
        for role in [Role::Owner, Role::Admin] {
            assert!(allows(
                role,
                Scope::Installation,
                Resource::People,
                Action::Write
            ));
            assert!(allows(role, Scope::Preview, Resource::Chat, Action::Write));
            assert!(allows(
                role,
                Scope::Production,
                Resource::Publish,
                Action::Write
            ));
        }
    }

    #[test]
    fn editors_edit_content_and_publish_only() {
        let editor = Role::Editor;
        assert!(allows(
            editor,
            Scope::Preview,
            Resource::Content,
            Action::Write
        ));
        assert!(allows(
            editor,
            Scope::Preview,
            Resource::Publish,
            Action::Write
        ));
        for resource in [
            Resource::People,
            Resource::Activity,
            Resource::Chat,
            Resource::Settings,
        ] {
            for action in [Action::Read, Action::Write] {
                assert!(
                    !allows(editor, Scope::Installation, resource, action),
                    "{resource:?}"
                );
                assert!(
                    !allows(editor, Scope::Preview, resource, action),
                    "{resource:?}"
                );
            }
        }
    }

    #[test]
    fn roles_round_trip() {
        for role in [Role::Owner, Role::Admin, Role::Editor] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        assert_eq!(Role::parse("root"), None);
    }

    #[test]
    fn capability_lists_follow_the_table() {
        let editor = capabilities(Role::Editor);
        assert!(!editor.iter().any(|c| c.resource == Resource::People));
        assert!(editor.contains(&Capability {
            resource: Resource::Content,
            action: Action::Write
        }));
        assert_eq!(capabilities(Role::Admin), capabilities(Role::Owner));
    }
}
