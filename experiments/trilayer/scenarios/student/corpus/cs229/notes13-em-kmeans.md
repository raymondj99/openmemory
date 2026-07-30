# Notes 13: k-means, mixtures of Gaussians, EM

K-means is coordinate descent on the distortion objective. Mixtures
of Gaussians add soft assignments; expectation maximization
alternates responsibilities (E-step) and parameter updates (M-step),
monotonically improving the evidence lower bound. The lower-bound
derivation via Jensen's inequality is a favorite exam question.
