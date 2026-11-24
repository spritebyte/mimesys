# IPU test pictures

64×48, one noisy `testsrc2` frame (`testsrc2=size=64x48:rate=25,noise=alls=40:allf=t`),
encoded with ffmpeg; each `.yuv` is ffmpeg's own decode (`-f rawvideo -pix_fmt yuv420p`).

| File | Encoder options |
|---|---|
| a.m2v | mpeg2video -q:v 2 -g 1 |
| b.m2v | mpeg2video -q:v 3 -qmax 28 -g 1 -intra_vlc 1 -alternate_scan 1 -non_linear_quant 1 -dc 10 |
| c.m1v | mpeg1video -q:v 2 -g 1 |
| d.m2v | mpeg2video -q:v 1 -qmin 1 -g 1 -intra_matrix 8,9,…,22 (rows rising by 1) |
| x1.m2v | mpeg2video -q:v 3 -g 1 -intra_vlc 1 |
| x2.m2v | mpeg2video -q:v 3 -g 1 -alternate_scan 1 (interlaced: field DCT bits, 64 coded lines) |
| p.m2v | mpeg2video -q:v 3 -g 2 -bf 0 -motion_est zero, 2 frames (I, P) |
