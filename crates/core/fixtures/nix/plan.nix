{
  failing = derivation {
    name = "failing-5.0";
    system = "x86_64-linux";
    builder = "/bin/sh";
    args = [ "-c" "exit 1" ];
  };
  missing = let collection = { hello = 1; }; in collection.ripgrep2;
}
