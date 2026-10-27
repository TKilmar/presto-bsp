include!("../nursery/version.rs");

fn main() {
    export_git_version();
    slint_build::compile("ui/desktop.slint").unwrap();
}
