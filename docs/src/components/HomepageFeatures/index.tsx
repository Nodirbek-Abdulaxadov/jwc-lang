import clsx from 'clsx';
import Heading from '@theme/Heading';
import styles from './styles.module.css';

type FeatureItem = {
  title: string;
  Svg: React.ComponentType<React.ComponentProps<'svg'>>;
  description: JSX.Element;
};

const FeatureList: FeatureItem[] = [
  {
    title: 'SQL is native',
    Svg: require('@site/static/img/undraw_docusaurus_mountain.svg').default,
    description: (
      <>
        <code>select</code>, <code>insert</code>, <code>update</code>,
        <code>delete</code> are real syntax. Columns and FK targets are
        checked at compile time — typos fail before the server starts.
      </>
    ),
  },
  {
    title: 'Backend out of the box',
    Svg: require('@site/static/img/undraw_docusaurus_tree.svg').default,
    description: (
      <>
        HTTP/2 + WebSocket via axum, JWT, Argon2 password hashing, in-memory
        cache, SMTP email, in-process job queue, migrations with diff and
        rollback. One <code>jwc</code> binary, no framework on top.
      </>
    ),
  },
  {
    title: 'One language, one CLI',
    Svg: require('@site/static/img/undraw_docusaurus_react.svg').default,
    description: (
      <>
        Routes, middleware, validation, transactions, background jobs,
        navigation properties — language features, not framework bolt-ons.
        <code>jwc check / test / lint / migrate / serve --watch</code>.
      </>
    ),
  },
];

function Feature({title, Svg, description}: FeatureItem) {
  return (
    <div className={clsx('col col--4')}>
      <div className="text--center">
        <Svg className={styles.featureSvg} role="img" />
      </div>
      <div className="text--center padding-horiz--md">
        <Heading as="h3">{title}</Heading>
        <p>{description}</p>
      </div>
    </div>
  );
}

export default function HomepageFeatures(): JSX.Element {
  return (
    <section className={styles.features}>
      <div className="container">
        <div className="row">
          {FeatureList.map((props, idx) => (
            <Feature key={idx} {...props} />
          ))}
        </div>
      </div>
    </section>
  );
}
