# Calibration metric

We report expected calibration error with fifteen equal-mass bins,
plus the reliability diagram. Equal-width binning was rejected: with
confident models most predictions land in the top bin and the metric
collapses. Confidence is read from token logprobs when the API
exposes them, otherwise from a verbalized 0-100 self-estimate, and
the two sources are never compared on one leaderboard.
