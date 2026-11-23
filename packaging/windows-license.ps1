param([Parameter(Mandatory)][string]$Destination)

$license = Get-Content -LiteralPath LICENSE -Raw
$paragraphs = ($license.Trim() -split '(?:\r?\n){2,}') | ForEach-Object {
  ($_ -replace '\r?\n', ' ').Replace('\', '\\').Replace('{', '\{').Replace('}', '\}')
}
$text = '{\rtf1\ansi\deff0{\fonttbl{\f0 Arial;}}\f0\fs18 ' + ($paragraphs -join '\par\par ') + '}'
[IO.File]::WriteAllText($Destination, $text, [Text.Encoding]::ASCII)
$text
