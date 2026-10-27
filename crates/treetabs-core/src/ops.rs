//! Tree operations. Every change to a stored tree is one of these, so it can
//! be written to the journal, replayed, sent to other processes and undone.

use crate::model::{EditError, Node, NodeId, NodeKind, TrashItem, Tree};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// All node fields except `id` and `children`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeFields {
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_favicon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub data: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub marks: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Node {
    pub fn fields(&self) -> NodeFields {
        NodeFields {
            kind: self.kind.clone(),
            title: self.title.clone(),
            url: self.url.clone(),
            favicon: self.favicon.clone(),
            custom_title: self.custom_title.clone(),
            custom_favicon: self.custom_favicon.clone(),
            note: self.note.clone(),
            collapsed: self.collapsed,
            data: self.data.clone(),
            marks: self.marks.clone(),
            extra: self.extra.clone(),
        }
    }

    pub fn set_fields(&mut self, f: NodeFields) {
        self.kind = f.kind;
        self.title = f.title;
        self.url = f.url;
        self.favicon = f.favicon;
        self.custom_title = f.custom_title;
        self.custom_favicon = f.custom_favicon;
        self.note = f.note;
        self.collapsed = f.collapsed;
        self.data = f.data;
        self.marks = f.marks;
        self.extra = f.extra;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Insert a new subtree. All ids in it must be new.
    Insert {
        parent: NodeId,
        index: usize,
        node: Box<Node>,
    },
    /// Remove a subtree for good. Only used to undo an `Insert`.
    Delete { id: NodeId },
    /// Move a subtree. `index` is counted after the node is taken out.
    Move {
        id: NodeId,
        parent: NodeId,
        index: usize,
    },
    /// Replace a node's fields (not its children).
    Update { id: NodeId, fields: Box<NodeFields> },
    /// Move a subtree into the trash.
    Trash { id: NodeId, at: i64 },
    /// Put a trashed subtree back.
    Restore {
        id: NodeId,
        parent: NodeId,
        index: usize,
    },
    /// Drop trash items removed before `before` (ms). Cannot be undone.
    PurgeTrash { before: i64 },
}

impl Op {
    /// Short name for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Op::Insert { .. } => "insert",
            Op::Delete { .. } => "delete",
            Op::Move { .. } => "move",
            Op::Update { .. } => "update",
            Op::Trash { .. } => "trash",
            Op::Restore { .. } => "restore",
            Op::PurgeTrash { .. } => "purge_trash",
        }
    }
}

impl Tree {
    /// Apply `op`. On success returns the op that undoes it (`None` for ops
    /// that cannot be undone). On error the tree is unchanged.
    ///
    /// # Errors
    ///
    /// When the op does not fit the tree (unknown id, id already used, index
    /// out of range, moving into itself); the tree is then unchanged.
    pub fn apply(&mut self, op: &Op) -> Result<Option<Op>, EditError> {
        match op {
            Op::Insert {
                parent,
                index,
                node,
            } => self.insert(*parent, *index, node),
            Op::Delete { id } => {
                let (parent, index) = self.locate_checked(*id)?;
                let node = self.root.remove(*id).ok_or(EditError::NotFound(*id))?;
                Ok(Some(Op::Insert {
                    parent,
                    index,
                    node: Box::new(node),
                }))
            }
            Op::Move { id, parent, index } => self.move_node(*id, *parent, *index),
            Op::Update { id, fields } => {
                let n = self.root.find_mut(*id).ok_or(EditError::NotFound(*id))?;
                let old = n.fields();
                n.set_fields((**fields).clone());
                Ok(Some(Op::Update {
                    id: *id,
                    fields: Box::new(old),
                }))
            }
            Op::Trash { id, at } => {
                let (parent, index) = self.locate_checked(*id)?;
                let node = self.root.remove(*id).expect("located above");
                self.trash.push(TrashItem {
                    removed_at: *at,
                    parent,
                    index,
                    node,
                });
                Ok(Some(Op::Restore {
                    id: *id,
                    parent,
                    index,
                }))
            }
            Op::Restore { id, parent, index } => self.restore(*id, *parent, *index),
            Op::PurgeTrash { before } => {
                self.trash.retain(|t| t.removed_at >= *before);
                Ok(None)
            }
        }
    }

    fn insert(
        &mut self,
        parent: NodeId,
        index: usize,
        node: &Node,
    ) -> Result<Option<Op>, EditError> {
        let mut clash = None;
        node.walk(&mut |n, _| {
            if clash.is_none()
                && (self.root.find(n.id).is_some() || self.trash_index(n.id).is_some())
            {
                clash = Some(n.id);
            }
        });
        if let Some(id) = clash {
            return Err(EditError::DuplicateId(id));
        }
        let p = self
            .root
            .find_mut(parent)
            .ok_or(EditError::NotFound(parent))?;
        if index > p.children.len() {
            return Err(EditError::BadIndex {
                index,
                len: p.children.len(),
            });
        }
        p.children.insert(index, node.clone());
        node.walk(&mut |n, _| NodeId::bump_above(n.id));
        Ok(Some(Op::Delete { id: node.id }))
    }

    fn move_node(
        &mut self,
        id: NodeId,
        parent: NodeId,
        index: usize,
    ) -> Result<Option<Op>, EditError> {
        let (old_parent, old_index) = self.locate_checked(id)?;
        let moving = self.root.find(id).ok_or(EditError::NotFound(id))?;
        if moving.contains(parent) {
            return Err(EditError::IntoSelf);
        }
        let p = self.root.find(parent).ok_or(EditError::NotFound(parent))?;
        // The index counts the siblings after the node is taken out.
        let new_len = p.children.len() - usize::from(old_parent == parent);
        if index > new_len {
            return Err(EditError::BadIndex {
                index,
                len: new_len,
            });
        }
        let node = self.root.remove(id).expect("located above");
        let p = self.root.find_mut(parent).expect("checked above");
        p.children.insert(index, node);
        Ok(Some(Op::Move {
            id,
            parent: old_parent,
            index: old_index,
        }))
    }

    fn restore(
        &mut self,
        id: NodeId,
        parent: NodeId,
        index: usize,
    ) -> Result<Option<Op>, EditError> {
        let ti = self.trash_index(id).ok_or(EditError::NotFound(id))?;
        let p = self
            .root
            .find_mut(parent)
            .ok_or(EditError::NotFound(parent))?;
        if index > p.children.len() {
            return Err(EditError::BadIndex {
                index,
                len: p.children.len(),
            });
        }
        let item = self.trash.remove(ti);
        let at = item.removed_at;
        p.children.insert(index, item.node);
        Ok(Some(Op::Trash { id, at }))
    }

    /// Position of a trashed subtree (by its root id).
    pub fn trash_index(&self, id: NodeId) -> Option<usize> {
        self.trash.iter().position(|t| t.node.id == id)
    }

    fn locate_checked(&self, id: NodeId) -> Result<(NodeId, usize), EditError> {
        if id == self.root.id {
            return Err(EditError::Root);
        }
        self.root.locate(id).ok_or(EditError::NotFound(id))
    }
}

/// Current time in ms since the epoch.
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// Current time in ms since the epoch.
#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(t: &str) -> Node {
        let mut n = Node::new(NodeKind::SavedTab);
        n.title = Some(t.into());
        n
    }

    fn snapshot(t: &Tree) -> String {
        serde_json::to_string(t).unwrap()
    }

    #[test]
    fn every_op_undoes() {
        let mut t = Tree::default();
        let mut w = Node::new(NodeKind::SavedWindow);
        let a = tab("a");
        let aid = a.id;
        w.children.push(a);
        let wid = w.id;
        let root = t.root.id;
        let before = snapshot(&t);

        let mut undo = Vec::new();
        let ops = vec![
            Op::Insert {
                parent: root,
                index: 0,
                node: Box::new(w),
            },
            Op::Insert {
                parent: wid,
                index: 1,
                node: Box::new(tab("b")),
            },
            Op::Move {
                id: aid,
                parent: root,
                index: 1,
            },
            Op::Update {
                id: aid,
                fields: Box::new({
                    let mut f = tab("x").fields();
                    f.collapsed = true;
                    f
                }),
            },
            Op::Trash { id: aid, at: 5 },
        ];
        for op in &ops {
            undo.push(t.apply(op).unwrap().unwrap());
        }
        assert_eq!(t.trash.len(), 1);
        for op in undo.iter().rev() {
            t.apply(op).unwrap();
        }
        assert_eq!(snapshot(&t), before);
    }

    #[test]
    fn errors_leave_tree_unchanged() {
        let mut t = Tree::default();
        let a = tab("a");
        let aid = a.id;
        t.apply(&Op::Insert {
            parent: t.root.id,
            index: 0,
            node: Box::new(a.clone()),
        })
        .unwrap();
        let before = snapshot(&t);
        assert!(matches!(
            t.apply(&Op::Insert {
                parent: t.root.id,
                index: 0,
                node: Box::new(a)
            }),
            Err(EditError::DuplicateId(_))
        ));
        assert!(
            t.apply(&Op::Move {
                id: aid,
                parent: t.root.id,
                index: 5
            })
            .is_err()
        );
        assert!(
            t.apply(&Op::Move {
                id: aid,
                parent: aid,
                index: 0
            })
            .is_err()
        );
        assert!(
            t.apply(&Op::Trash {
                id: t.root.id,
                at: 0
            })
            .is_err()
        );
        assert_eq!(snapshot(&t), before);
    }

    #[test]
    fn op_json_shape() {
        let op = Op::Move {
            id: NodeId(3),
            parent: NodeId(1),
            index: 2,
        };
        assert_eq!(
            serde_json::to_value(&op).unwrap(),
            serde_json::json!({"op": "move", "id": 3, "parent": 1, "index": 2})
        );
    }
}
