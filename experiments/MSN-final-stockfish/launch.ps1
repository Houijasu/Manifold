Set-Location 'C:\Users\Samaritan\Projects\Manifold'
& '.\harness\run_match.ps1' `
    -OutDir 'experiments\MSN-final-stockfish' `
    -Purpose 'M4-F2 / A-SF-001 part 2: mission-final build vs Stockfish 18 at 1T, replicating experiments/MSN-F3-stockfish-baseline EXACTLY (TC 8+0.08, Hash 64, 1T, UHO_4060_v4, 150 rounds, seed 20260805) so the two matches are paired at the opening level and their delta is the mission headline gap change.' `
    -AName 'mission-final' -ACmd '.\baselines\mission-final\manifold.exe' `
    -BName 'stockfish' -BCmd 'C:\Users\Samaritan\bin\stockfish.exe' `
    -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 20260805
exit $LASTEXITCODE
