param([string[]]$Spaces=@('grok-image-pool','grok-image-script-run','grok-chat-space','grok-imagine-gallery-space','grok-image-rs','personaxi-agent-hub','simtrader-hma-mean-reversion-live'),[switch]$Upload)
$ErrorActionPreference='Stop'
$taskRoot=Resolve-Path "$PSScriptRoot/../.."
$taskPrivate=Join-Path $taskRoot 'projects/nexus-agent/private/space-deploy'
$taskNodes=Get-Content "$taskPrivate/nodes.json" -Raw|ConvertFrom-Json
# HF's own CLI refreshes the current OAuth access token; never copy its refresh token.
$null=hf auth whoami --format json
if($LASTEXITCODE -ne 0){throw 'HF login unavailable'}
$taskHfHome=if($env:HF_HOME){$env:HF_HOME}else{Join-Path $env:USERPROFILE '.cache/huggingface'}
$taskToken=(Get-Content (Join-Path $taskHfHome 'token') -Raw).Trim()
foreach($taskSlug in $Spaces){
 if($taskSlug -eq 'go-emo'){throw 'go-emo is excluded by the owner'}
 $taskNode=$taskNodes|Where-Object {$_.space -eq "yuchanahn2/$taskSlug"}
 if(!$taskNode){throw 'Unknown target; this script cannot create a new Space'}
 $taskPeers=@($taskNodes|Where-Object {$_.node -ne $taskNode.node}|ForEach-Object {@{node_id=$_.node;url=$_.url;token=$_.peer_token}})|ConvertTo-Json -Compress -Depth 3
 $taskGoal="NEXUS continuous cloud expansion mission. Your node is $($taskNode.node). Coordinate with configured peers through nexus peer board and durable DM tools; consult nexus/expansion headers and selectively read bodies. Own a distinct reproducible Rust research task for supplier discovery, real tool-call evaluations, cloud portability, or swarm cooperation. Begin by publishing your node identity and testing one shell operation; never claim results without evidence. Do not duplicate other nodes' work. Keep the protected core and credentials inaccessible; improve tools/provider code in /workspace/projects or /workspace/work. New routes may use protected existing-provider credentials but never expose them. No Astra, no paid resources, no arbitrary deployment credentials. Continually produce working Rust, model measurements or falsifiable findings, checkpoint and coordinate. This Space uses a separate-UID process executor, not nested Docker; per-agent external service ports are unsupported. Never wait solely for elapsed time. Source distribution is https://github.com/yuchanahn/NEXUS with deploy/space as the self-contained release; new nodes still need owner-provisioned private credentials and peers."
 $taskSettings=@{NEXUS_NODE_ID=$taskNode.node;NEXUS_ADMIN_TOKEN=$taskNode.admin_token;NEXUS_PEER_TOKEN=$taskNode.peer_token;NEXUS_PEERS=$taskPeers;NEXUS_HF_TOKEN=$taskToken;NEXUS_STATE_DATASET='yuchanahn2/nexus-swarm-state';NEXUS_GOAL=$taskGoal}
 $taskLines=@($taskSettings.GetEnumerator()|ForEach-Object {$_.Key+"='"+$_.Value.Replace("'", "")+"'"})
 $taskFile=Join-Path $taskPrivate "$taskSlug.env"
 $taskLines|Set-Content $taskFile
 $null=hf spaces secrets add $taskNode.space --secrets-file $taskFile --format quiet
 if($LASTEXITCODE -ne 0){throw "Secret provisioning failed: $taskSlug"}
 if($Upload){
  # Existing paths are updated, not deleted; rollback revisions are recorded separately.
  hf upload $taskNode.space $PSScriptRoot . --type space --exclude Provision-Spaces.ps1 --commit-message 'Deploy Rust NEXUS cloud research node' --format quiet
  if($LASTEXITCODE -ne 0){throw "Upload failed: $taskSlug"}
 }
 "Provisioned $($taskNode.space); credentials not printed."
}
