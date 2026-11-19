class ValidationError extends Error {
  constructor(message) {
    super(message);
    this.name = 'ValidationError';
  }
}
print(new ValidationError('x').name);
throw new ValidationError('invalid input');
