Set-Location 'C:\Users\Samaritan\Projects\Manifold'
& '.\harness\run_match.ps1' `
    -OutDir 'experiments\MSN-NNUE-confirm' `
    -Purpose 'M2 confirmation: the combined NNUE speed package (Finny tables, lazy accumulator updates, threat-discovery skip, depth cap) vs the mission-start build, checking that the +8% 1T NPS gain costs no strength (A-NNUE-002).' `
    -AName 'm2-nnue' -ACmd '.\target\release\manifold.exe' `
    -BName 'mission-start' -BCmd '.\baselines\mission-start\manifold.exe' `
    -TC '8+0.08' -Hash 64 -Rounds 150 -Seed 20260806
exit $LASTEXITCODE
