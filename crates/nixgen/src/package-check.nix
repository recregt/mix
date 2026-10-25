pkgs: names:
let
  asList = value: if builtins.isList value then value else [ value ];
  metaOf =
    package: if builtins.isAttrs (package.meta or null) then package.meta else { };
  unfree =
    meta:
    builtins.any (license: builtins.isAttrs license && !(license.free or true)) (
      asList (meta.license or [ ])
    );
  forced = value: builtins.tryEval (builtins.deepSeq value value);
  check =
    name:
    if !(pkgs ? ${name}) then
      {
        inherit name;
        known = false;
      }
    else
      let
        reasons = forced (
          let
            meta = metaOf pkgs.${name};
          in
          {
            unfree = unfree meta;
            broken = (meta.broken or false) == true;
            insecure = (meta.knownVulnerabilities or [ ]) != [ ];
          }
        );
        evaluated = forced (
          let
            package = pkgs.${name};
            meta = metaOf package;
            outputs =
              if builtins.isList (meta.outputsToInstall or null) then meta.outputsToInstall else [ "out" ];
          in
          {
            derivation = package.drvPath;
            outputs = map (output: {
              inherit output;
              path = package.${output}.outPath;
            }) outputs;
          }
        );
        reason = field: reasons.success && reasons.value.${field};
      in
      {
        inherit name;
        known = true;
        evaluates = evaluated.success;
        unfree = reason "unfree";
        broken = reason "broken";
        insecure = reason "insecure";
        derivation = if evaluated.success then evaluated.value.derivation else null;
        outputs = if evaluated.success then evaluated.value.outputs else [ ];
      };
in
map check names
