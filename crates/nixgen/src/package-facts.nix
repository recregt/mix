pkgs: names:
let
  text = value: if builtins.isString value then value else null;
  license =
    value:
    if builtins.isString value then
      value
    else if builtins.isAttrs value then
      value.spdxId or value.shortName or value.fullName or null
    else
      null;
  licenses =
    value:
    builtins.filter (found: found != null) (
      map license (
        if builtins.isList value then
          value
        else if value == null then
          [ ]
        else
          [ value ]
      )
    );
  attrs = value: if builtins.isAttrs value then value else { };
  describe =
    name:
    let
      package = pkgs.${name};
      meta = attrs (package.meta or { });
    in
    if builtins.isAttrs package then
      {
        inherit name;
        pname = text (package.pname or null);
        version = text (package.version or null);
        storeName = text (package.name or null);
        outputs = if builtins.isList (meta.outputsToInstall or null) then meta.outputsToInstall else [ "out" ];
        description = text (meta.description or null);
        homepage = text (meta.homepage or null);
        licenses = licenses (meta.license or null);
        mainProgram = text (meta.mainProgram or null);
        changelog = text (meta.changelog or null);
      }
    else
      { inherit name; };
  attempt =
    name:
    if pkgs ? ${name} then
      let
        tried = builtins.tryEval (
          let
            described = describe name;
          in
          builtins.deepSeq described described
        );
      in
      if tried.success then tried.value else { inherit name; }
    else
      { inherit name; };
in
map attempt names
