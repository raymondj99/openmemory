# Notes 3: logistic regression and GLMs

The sigmoid maps scores to probabilities; the log-likelihood is
concave so Newton's method converges in a handful of steps. Deriving
the gradient gives the same update shape as linear regression, which
the professor kept emphasizing: it is a property of the exponential
family, not a coincidence. Softmax regression generalizes to
multiclass and its derivative is the one reused later for neural
network output layers.
