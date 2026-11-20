use std::fs;
use std::path::{Path, PathBuf};
use eframe::egui;

pub struct FileNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// `None` înseamnă că subdirectoarele nu au fost citite încă de pe disc (lazy loading).
    pub children: Option<Vec<FileNode>>,
}

impl FileNode {
    pub fn from_path(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| path.display().to_string());
        let is_dir = path.is_dir();

        Self {
            path,
            name,
            is_dir,
            children: None,
        }
    }

    /// Citește conținutul directorului și sortează: directoare primele, apoi fișiere (alfabetic).
    pub fn load_children(&mut self) {
        if !self.is_dir {
            return;
        }

        let mut entries = Vec::new();
        if let Ok(read_dir) = fs::read_dir(&self.path) {
            for entry in read_dir.flatten() {
                entries.push(Self::from_path(entry.path()));
            }
        }

        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        self.children = Some(entries);
    }

    /// Reîncarcă arborele păstrând starea directoarelor deja expandate.
    pub fn refresh(&mut self) {
        if !self.is_dir {
            return;
        }

        let previously_expanded: Vec<PathBuf> = self
            .children
            .as_ref()
            .map(|kids| {
                kids.iter()
                    .filter(|k| k.is_dir && k.children.is_some())
                    .map(|k| k.path.clone())
                    .collect()
            })
            .unwrap_or_default();

        self.load_children();

        if let Some(kids) = &mut self.children {
            for kid in kids.iter_mut() {
                if previously_expanded.contains(&kid.path) {
                    kid.refresh();
                }
            }
        }
    }

    fn render(
        &mut self,
        ui: &mut egui::Ui,
        active_file: Option<&Path>,
        clicked_file: &mut Option<PathBuf>,
    ) {
        if self.is_dir {
            egui::CollapsingHeader::new(format!("📁 {}", self.name))
                .id_salt(&self.path)
                .show(ui, |ui| {
                    if self.children.is_none() {
                        self.load_children();
                    }

                    if let Some(children) = &mut self.children {
                        if children.is_empty() {
                            ui.weak("(gol)");
                        } else {
                            for child in children.iter_mut() {
                                child.render(ui, active_file, clicked_file);
                            }
                        }
                    }
                });
        } else {
            let is_selected = active_file.is_some_and(|active| active == self.path);
            let label = format!("📄 {}", self.name);

            if ui.selectable_label(is_selected, label).clicked() {
                *clicked_file = Some(self.path.clone());
            }
        }
    }
}

#[derive(Default)]
pub struct FileExplorer {
    pub root: Option<FileNode>,
}

pub enum ExplorerAction {
    None,
    OpenFolderRequested,
    FileClicked(PathBuf),
}

impl FileExplorer {
    pub fn set_root(&mut self, path: PathBuf) {
        let mut node = FileNode::from_path(path);
        node.load_children();
        self.root = Some(node);
    }

    pub fn refresh(&mut self) {
        if let Some(root) = &mut self.root {
            root.refresh();
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, active_file: Option<&Path>) -> ExplorerAction {
        let mut action = ExplorerAction::None;

        ui.horizontal(|ui| {
            ui.strong("EXPLORER");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.root.is_some() {
                    if ui.small_button("↻").on_hover_text("Reîncarcă arborele").clicked() {
                        self.refresh();
                    }
                    if ui.small_button("📂").on_hover_text("Schimbă directorul").clicked() {
                        action = ExplorerAction::OpenFolderRequested;
                    }
                }
            });
        });

        ui.separator();

        if let Some(root) = &mut self.root {
            ui.strong(root.name.to_uppercase());
            ui.add_space(4.0);

            egui::ScrollArea::both()
                .id_salt("explorer_tree_scroll")
                .show(ui, |ui| {
                    let mut clicked_file = None;
                    if let Some(children) = &mut root.children {
                        for child in children.iter_mut() {
                            child.render(ui, active_file, &mut clicked_file);
                        }
                    }
                    if let Some(path) = clicked_file {
                        action = ExplorerAction::FileClicked(path);
                    }
                });
        } else {
            ui.add_space(12.0);
            ui.label("Niciun director deschis.");
            ui.add_space(8.0);
            if ui.button("Open Folder").clicked() {
                action = ExplorerAction::OpenFolderRequested;
            }
        }

        action
    }

    
}