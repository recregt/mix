fn main() {
    let command = "mix install";
    let _ = mix_ui::phrase!("`{command}` can't be run as root");
    let _ = mix_ui::note!("`mix` needs it to work");
    let _ = mix_ui::help!("run it again without sudo");
    let _ = mix_ui::help_parts!["wait for it to finish, then run `", command, "` again"];
    let _ = mix_ui::sentence!("`mix` has not been set up on this machine.");
    let _ = mix_ui::prose!("It stopped. Nothing was changed.");
    let _ = mix_ui::instruction!("run `mix repair`");
}
