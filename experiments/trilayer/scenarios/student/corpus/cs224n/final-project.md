# CS224N final project: calibration of finetuned QA models

Question: does finetuning a pretrained transformer for extractive QA
preserve calibration? Method: expected calibration error with
equal-mass bins on SQuAD dev, before and after finetuning, plus
temperature scaling as the fix. Uses the softmax confidence directly,
connecting back to the CS229 logistic regression view of probability
outputs.
