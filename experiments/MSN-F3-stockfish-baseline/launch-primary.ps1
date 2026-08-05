Set-Location 'C:\Users\Samaritan\Projects\Manifold'
& '.\harness\run_match.ps1' `
    -OutDir 'experiments\MSN-F3-stockfish-baseline' `
    -Purpose 'M1 anchor benchmark: mission-start build vs Stockfish 18 at 1T, quantifying the starting strength gap (A-SF-001). M4 must replicate these conditions exactly.' `
    -AName 'manifold-mission-start' -ACmd '.\baselines\mission-start\manifold.exe' `
    -BName 'stockfish' -BCmd 'C:\Users\Samaritan\bin\stockfish.exe' `
    -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 20260805
exit $LASTEXITCODE
