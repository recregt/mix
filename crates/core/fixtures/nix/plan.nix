let
  system = "x86_64-linux";
  step = name: attrs:
    derivation ({
      inherit name system;
      builder = "/bin/sh";
      args = [ "-c" "echo ${name} > $out" ];
    } // attrs);

  shell = step "shell-5.3" { };
  leaf = step "leaf-1.0" { };
  library = step "library-2.1" { inputs = [ leaf ]; };
  package = step "package-3.2" { inputs = [ library ]; };
  tool = step "tool-4.0" { };
  wrapper = step "tool-wrapper" {
    inputs = [ tool ];
    preferLocalBuild = true;
    allowSubstitutes = false;
  };
  documentation = step "options.json" { };
  profile = step "home-manager-path" {
    __structuredAttrs = true;
    shell = "${shell}";
    chosenOutputs = [
      { paths = [ "${package}" ]; priority = 5; }
      { paths = [ "${wrapper}" ]; priority = 5; }
      { paths = [ "${library}" ]; priority = 5; }
    ];
    preferLocalBuild = true;
    allowSubstitutes = false;
  };
  generation = step "home-manager-generation" {
    __structuredAttrs = true;
    inputs = [ profile documentation ];
    preferLocalBuild = true;
    allowSubstitutes = false;
  };
  failing = derivation {
    name = "failing-5.0";
    inherit system;
    builder = "/bin/sh";
    args = [ "-c" "exit 1" ];
  };
in
{
  inherit generation leaf failing;
}
