// The uncaught error's constructor name and phase are compared.
print('before');
const value = undefined;
value.property;
print('never printed');
