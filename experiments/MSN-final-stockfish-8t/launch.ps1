Set-Location 'C:\Users\Samaritan\Projects\Manifold'
& '.\harness\run_match.ps1' `
    -OutDir 'experiments\MSN-final-stockfish-8t' `
    -Purpose 'M4-F2 / A-SF-001 8T addendum: mission-final vs Stockfish 18 with Threads=8 on BOTH engines under the multi-thread harness rules (no -use-affinity, concurrency 1). TC/Hash/book/seed identical to the 1T anchor (8+0.08, Hash 64, UHO_4060_v4, seed 20260805) so thread count is the only variable; 120 games (60 rounds) because 8T at concurrency 1 monopolizes all 8 P-cores.' `
    -AName 'mission-final' -ACmd '.\baselines\mission-final\manifold.exe' -AThreads 8 `
    -BName 'stockfish' -BCmd 'C:\Users\Samaritan\bin\stockfish.exe' -BThreads 8 `
    -TC '8+0.08' -Hash 64 -Rounds 60 -Seed 20260805
exit $LASTEXITCODE
