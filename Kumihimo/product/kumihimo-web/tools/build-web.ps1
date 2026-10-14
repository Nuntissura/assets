param(
    [string]$CargoTargetDir = $env:CARGO_TARGET_DIR,
    [string]$WasmBindgen = $env:KUMIHIMO_WASM_BINDGEN,
    [string]$OutputDir = '.local/web/pkg',
    [string]$ManifestOutput = '.local/web/build-manifest.json',
    [string]$Features = 'wasm,drops',
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
function Resolve-ProjectPath([string]$value) {
    if ([IO.Path]::IsPathRooted($value)) { return [IO.Path]::GetFullPath($value) }
    return [IO.Path]::GetFullPath((Join-Path $projectRoot $value))
}
function Assert-IgnoredOutput([string]$path) {
    $localRoot = [IO.Path]::GetFullPath((Join-Path $projectRoot '.local')) + [IO.Path]::DirectorySeparatorChar
    if (-not $path.StartsWith($localRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Generated output must be inside this project .local ignored directory.'
    }
}
if (-not $CargoTargetDir) { $CargoTargetDir = '.local/cargo-target' }
if (-not $WasmBindgen) { throw 'Configure -WasmBindgen or KUMIHIMO_WASM_BINDGEN with the verified matching CLI.' }
$targetRoot = Resolve-ProjectPath $CargoTargetDir
$outputRoot = Resolve-ProjectPath $OutputDir
$manifestPath = Resolve-ProjectPath $ManifestOutput
Assert-IgnoredOutput $outputRoot
Assert-IgnoredOutput $manifestPath
$cli = (Get-Command $WasmBindgen -ErrorAction Stop).Source
$cliVersion = (& $cli --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $cliVersion -ne 'wasm-bindgen 0.2.128') { throw "Exact wasm-bindgen 0.2.128 required; found $cliVersion" }
$cargoVersion = (& cargo --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw 'cargo version failed' }
$rustVersion = (& rustc --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw 'rustc version failed' }
$workspaceManifest = Join-Path $projectRoot 'product/Cargo.toml'
if (-not $SkipBuild) {
    & cargo build --manifest-path $workspaceManifest -p kumihimo-web --features $Features --target wasm32-unknown-unknown --target-dir $targetRoot --locked --offline -j 2
    if ($LASTEXITCODE -ne 0) { throw 'WASM build failed; no bindings generated.' }
}
$wasm = Join-Path $targetRoot 'wasm32-unknown-unknown/debug/kumihimo_web.wasm'
if (-not (Test-Path -LiteralPath $wasm -PathType Leaf)) { throw 'Selected target has no built WASM.' }
[IO.Directory]::CreateDirectory($outputRoot) | Out-Null
& $cli --target web --out-dir $outputRoot --out-name kumihimo_web $wasm
if ($LASTEXITCODE -ne 0) { throw 'Binding generation failed.' }
$sourceRoots = @('product/kumihimo-web','product/kumihimo-core','product/kumihimo-content','product/kumihimo-layout-json') | ForEach-Object { Join-Path $projectRoot $_ }
$sourceFiles = @($sourceRoots | ForEach-Object { Get-ChildItem -LiteralPath $_ -File -Recurse } | Where-Object { $_.Extension -in '.rs','.mjs','.html','.css','.py','.ps1','.toml' })
$sourceFiles += Get-Item -LiteralPath (Join-Path $projectRoot 'product/Cargo.toml'),(Join-Path $projectRoot 'product/Cargo.lock')
$sources = @($sourceFiles | Sort-Object FullName -Unique | ForEach-Object {
    [ordered]@{ path = [IO.Path]::GetRelativePath($projectRoot, $_.FullName); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
})
$generated = @('kumihimo_web.js','kumihimo_web_bg.wasm') | ForEach-Object {
    $file = Join-Path $outputRoot $_
    [ordered]@{ path = [IO.Path]::GetRelativePath($projectRoot, $file); sha256 = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant(); bytes = (Get-Item -LiteralPath $file).Length }
}
$result = [ordered]@{
    version = 1; cargo = $cargoVersion; rustc = $rustVersion; wasm_bindgen = $cliVersion; build_executed = (-not $SkipBuild)
    cli_sha256 = (Get-FileHash -LiteralPath $cli -Algorithm SHA256).Hash.ToLowerInvariant()
    source_wasm_sha256 = (Get-FileHash -LiteralPath $wasm -Algorithm SHA256).Hash.ToLowerInvariant()
    target_dir = $targetRoot; generated = @($generated); sources = $sources
}
[IO.Directory]::CreateDirectory((Split-Path -Parent $manifestPath)) | Out-Null
[IO.File]::WriteAllText($manifestPath, ($result | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
$result | ConvertTo-Json -Depth 8
