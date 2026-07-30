# Notes 6: convolutional networks

Convolutions share weights across positions; parameter count depends
on kernel size and channels, not image size. Padding and stride
arithmetic. Pooling for translation tolerance. Classic
architectures: LeNet to ResNet, where skip connections fix vanishing
gradients in very deep stacks. Assignment 2 implements the forward
and backward pass for a conv layer by hand.
