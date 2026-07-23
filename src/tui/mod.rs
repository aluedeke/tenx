mod mouse;
mod overlay;

pub fn run_overlay(home: bool) -> anyhow::Result<()> {
    overlay::run(home)
}
