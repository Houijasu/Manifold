Set-Location 'C:\Users\Samaritan\Projects\Manifold'
& '.\harness\run_match.ps1' `
    -OutDir 'experiments\MSN-final-cumulative' `
    -Purpose 'M4-F2 headline: cumulative mission Elo -- baselines/mission-final (bench 41588, commit cec5d43) vs baselines/mission-start (bench 45036, commit 0012b36), 300 games 1T TC 8+0.08 Hash 64 (A-ELO-001).' `
    -AName 'mission-final' -ACmd '.\baselines\mission-final\manifold.exe' `
    -BName 'mission-start' -BCmd '.\baselines\mission-start\manifold.exe' `
    -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 20260806
exit $LASTEXITCODE
